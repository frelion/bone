use bone_app::{RequestId, SessionId, SubmitInput};

use crate::editor::{CursorMove, EditCommand};

use super::{
    HISTORY_CACHE_BYTES,
    answer::{self, AnswerDraft, RecoveryCandidate},
    model::*,
    panel,
    protocol::*,
};

#[cfg(test)]
use super::{ModelPanel, ModelScreen, Panel, ReaderPanel};

pub fn update(state: &mut UiState, event: UiEvent) -> Vec<Effect> {
    if matches!(event, UiEvent::CaretBlink) {
        if state.blinking_caret_active() {
            state.caret_visible = !state.caret_visible;
            state.dirty = true;
        }
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
        } => panel::connection_saved(state, request, session, error, notice, &mut effects),

        UiEvent::LoginChanged {
            request,
            state: login,
        } => panel::login_changed(state, request, login, &mut effects),
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
        } => panel::models_failed(state, session, request, error),
        UiEvent::ModelsLoaded {
            session,
            request,
            choices,
            profiles,
        } => panel::models_loaded(state, session, request, choices, profiles),
        UiEvent::ModelApplied {
            session,
            request,
            label,
            facts,
            error,
        } => panel::model_applied(state, session, request, label, facts, error, &mut effects),
        UiEvent::Action(action) => handle_action(state, action, &mut effects),
        UiEvent::WorkspaceOpened {
            label,
            rows,
            last_active,
            model_label,
        } => {
            state.workspace_label = Some(label);
            state.model_label = model_label;
            state.session_rows = rows;
            let preferred = last_active.filter(|candidate| {
                state
                    .session_rows
                    .iter()
                    .any(|row| row.id() == *candidate && !row.info().archived)
            });
            if let Some(session) = preferred.or_else(|| {
                state
                    .session_rows
                    .iter()
                    .find(|row| !row.info().archived)
                    .map(SessionNavRow::id)
            }) {
                select_session(state, session, &mut effects);
            }
        }
        UiEvent::SessionOpened {
            session,
            generation,
            snapshot,
            history,
        } => {
            if state
                .session_ui
                .get(&session)
                .is_some_and(|ui| ui.generation == generation)
            {
                merge_authoritative_session_info(state, session, &snapshot.session);
            }
            if let Some(ui) = current_generation_mut(state, session, generation) {
                if !ui.hydrated {
                    if ui.draft.is_empty() {
                        let revision = ui.draft.revision();
                        let cursor = snapshot.draft.len();
                        ui.draft
                            .reset_external(snapshot.draft.clone(), cursor, revision);
                    } else if !snapshot.draft.is_empty() && ui.draft.text() != snapshot.draft {
                        let merged = format!("{}\n{}", snapshot.draft, ui.draft.text());
                        let cursor = merged.len();
                        ui.draft.reconcile(merged, cursor);
                    }
                    ui.hydrated = true;
                }
                ui.snapshot = Some(snapshot);
                ui.transcript.open(history);
                if let Some(pending) = ui.bootstrap_submission.take() {
                    let input = SubmitInput {
                        request_id: pending.request_id,
                        text: pending.text.clone(),
                        reply_to: None,
                    };
                    ui.submitting = Some(pending);
                    effects.push(Effect::Submit { session, input });
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
            {
                panel::refresh_reader(state, &snapshot);
            }
            if state
                .session_ui
                .get(&session)
                .is_some_and(|ui| ui.generation == generation)
            {
                merge_authoritative_session_info(state, session, &snapshot.session);
            }
            if let Some(ui) = current_generation_mut(state, session, generation) {
                let through = snapshot.history_through;
                ui.snapshot = Some(snapshot);
                if let Some(after) = ui.transcript.session_changed(through) {
                    effects.push(Effect::LoadHistory {
                        session,
                        generation,
                        after,
                    });
                }
            }
        }
        UiEvent::HistoryLoaded {
            session,
            generation,
            page,
        } => {
            if let Some(ui) = current_generation_mut(state, session, generation)
                && let Some(after) = ui.transcript.history_loaded(page)
            {
                effects.push(Effect::LoadHistory {
                    session,
                    generation,
                    after,
                });
            }
        }
        UiEvent::OlderHistoryLoaded {
            session,
            generation,
            page,
        } => {
            if let Some(ui) = current_generation_mut(state, session, generation) {
                ui.transcript.older_history_loaded(page);
            }
        }
        UiEvent::RecentHistoryReloaded {
            session,
            generation,
            page,
        } => {
            if let Some(ui) = current_generation_mut(state, session, generation) {
                ui.transcript.recent_history_reloaded(page);
            }
        }
        UiEvent::PersistDraftsRequested => {
            for ui in state.session_ui.values() {
                if ui.hydrated && ui.draft.revision() > ui.saved_draft_revision {
                    effects.push(Effect::SaveDraft {
                        session: ui.id,
                        generation: ui.generation,
                        revision: ui.draft.revision(),
                        text: ui.draft.text().to_owned(),
                    });
                }
            }
        }
        UiEvent::RefreshOverviewRequested => {
            if state.overview_request.is_none() {
                let generation = state.generation();
                state.overview_request = Some(generation);
                effects.push(Effect::RefreshOverview { generation });
            }
        }
        UiEvent::OverviewLoaded {
            generation,
            mut rows,
        } => {
            if state.overview_request != Some(generation) {
                return effects;
            }
            state.overview_request = None;
            reconcile_overview_rows(state, &mut rows);
            state.session_rows = rows;
            if state.selected.is_some_and(|id| {
                !state
                    .session_rows
                    .iter()
                    .any(|row| row.id() == id && !row.info().archived)
            }) {
                state.selected = None;
                state.clear_title_edit();
                panel::dismiss(state, &mut effects);
                state.remove_title_focus();
                panel::refresh_model_label(state, &mut effects);
            }
            if state.session_candidate.is_some_and(|id| {
                !state
                    .session_rows
                    .iter()
                    .any(|row| row.id() == id && !row.info().archived)
            }) {
                state.session_candidate = state.selected;
                state.session_scroll = None;
            }
        }
        UiEvent::DraftSaved {
            session,
            generation,
            revision,
        } => {
            if let Some(ui) = current_generation_mut(state, session, generation)
                && revision >= ui.saved_draft_revision
            {
                ui.saved_draft_revision = revision;
            }
        }
        UiEvent::Submitted {
            session,
            request_id,
        } => {
            let manual_title = state.title_manual_intent.contains(&session);
            let mut auto_title = None;
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
                    if ui.draft.revision() == pending.draft_revision
                        && ui.draft.text() == pending.text
                    {
                        ui.draft.clear();
                        effects.push(Effect::SaveDraft {
                            session,
                            generation: ui.generation,
                            revision: ui.draft.revision(),
                            text: String::new(),
                        });
                    }
                    if !manual_title {
                        auto_title = Some((ui.generation, pending.text));
                    }
                }
            }
            if let Some((generation, first_input)) = auto_title {
                let request = state.generation();
                effects.push(Effect::AutoTitle {
                    session,
                    generation,
                    request,
                    first_input,
                });
            }
        }
        UiEvent::SubmitFailed {
            session,
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
                    let editor = std::mem::take(&mut state.orphan_draft);
                    state.orphan_draft.revision = editor.revision().wrapping_add(1);
                    editor
                });
                if transferred.is_none() {
                    clear_create_source(state, pending.source, &mut effects);
                }
                if state.session_row(info.id).is_none() {
                    state
                        .session_rows
                        .insert(0, SessionNavRow::provisional(info.clone()));
                }
                if state.focus == Focus::SessionTitle {
                    commit_title_edit(state, &mut effects);
                }
                select_session(state, info.id, &mut effects);
                if let Some(editor) = transferred
                    && let Some(ui) = state.session_ui.get_mut(&info.id)
                {
                    ui.draft = editor;
                    ui.bootstrap_submission = pending.first_input;
                }
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
            request,
            title,
        } => {
            let matches = state
                .title_renames
                .get(&session)
                .is_some_and(|queue| queue.pending.contains_key(&request));
            if !matches {
                return effects;
            }
            let (completed, newest) = {
                let queue = state
                    .title_renames
                    .get_mut(&session)
                    .expect("matching rename queue");
                let completed = queue
                    .pending
                    .remove(&request)
                    .expect("matching rename request");
                let newest = request > queue.applied;
                if newest {
                    queue.applied = request;
                    queue.confirmed = Some(title.clone());
                }
                (completed, newest)
            };
            if newest {
                set_committed_session_title(state, session, title.clone());
                if let Some(edit) = state
                    .title_edit
                    .as_mut()
                    .filter(|edit| edit.target == session)
                {
                    edit.original = title;
                    if edit.editor.text() == completed {
                        let revision = edit.editor.revision();
                        let cursor = edit.original.len();
                        edit.editor
                            .reset_external(edit.original.clone(), cursor, revision);
                    }
                }
                clear_title_rename_error(state, session);
            }
            dispatch_queued_title(state, session, &mut effects);
            finish_title_rename(state, session, &mut effects);
        }
        UiEvent::SessionRenameFailed {
            session,
            request,
            message,
        } => {
            let Some(queue) = state.title_renames.get_mut(&session) else {
                return effects;
            };
            let Some(failed) = queue.pending.remove(&request) else {
                return effects;
            };
            let superseded =
                request <= queue.applied || !queue.pending.is_empty() || queue.queued.is_some();
            dispatch_queued_title(state, session, &mut effects);
            if !superseded && !state.title_rename_pending(session) {
                let committed = committed_session_title(state, session).unwrap_or_default();
                if let Some(edit) = state
                    .title_edit
                    .as_mut()
                    .filter(|edit| edit.target == session)
                {
                    edit.original = committed.clone();
                    if edit.editor.text() == failed {
                        let revision = edit.editor.revision().wrapping_add(1);
                        edit.editor
                            .reset_external(committed.clone(), committed.len(), revision);
                    }
                }
                if state.selected == Some(session) && !state.quitting {
                    set_title_rename_error(state, session, message);
                }
            }
            finish_title_rename(state, session, &mut effects);
        }
        UiEvent::SessionAutoTitled {
            session,
            request: _,
            title,
        } => {
            if state.title_manual_intent.contains(&session) {
                return effects;
            }
            if state.session_ui.contains_key(&session) || state.session_row(session).is_some() {
                state.title_renames.entry(session).or_default().confirmed = Some(title.clone());
                set_committed_session_title(state, session, title.clone());
                let manual_pending = state.title_rename_pending(session);
                if let Some(edit) = state
                    .title_edit
                    .as_mut()
                    .filter(|edit| edit.target == session)
                    && edit.editor.revision() == 0
                    && !manual_pending
                {
                    edit.original = title.clone();
                    edit.editor = crate::editor::EditorBuffer::new(title);
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
                    ui.transcript.start_generation();
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
                            ui.transcript.older_history_failed();
                        }
                        OperationKind::LoadHistory => {
                            ui.transcript.history_failed();
                        }
                        OperationKind::ReloadRecentHistory => {
                            ui.transcript.recent_history_failed();
                        }
                        _ => {}
                    }
                }
                if kind == OperationKind::RefreshOverview {
                    state.overview_request = None;
                }
                if session.is_none() || state.selected == session {
                    state.status = Some(message);
                }
            }
        }
        UiEvent::Resized { width, height } => {
            state.dragging_divider = None;
            if !crate::layout::right_rail_available(width, height) {
                let center = state.last_center_focus();
                if state.focus == Focus::RightRail {
                    state.set_focus(center);
                    state.caret_visible = true;
                }
            }
        }
        UiEvent::CaretBlink => unreachable!("caret blink returns before event dispatch"),
    }
    trim_editor_history(state);
    trim_history_to_limit(state, HISTORY_CACHE_BYTES);
    effects
}

fn handle_action(state: &mut UiState, action: Action, effects: &mut Vec<Effect>) {
    state.caret_visible = true;
    let leaves_title = matches!(
        action,
        Action::OpenModels
            | Action::OpenHistory(_)
            | Action::OpenJob(_)
            | Action::SelectSession(_)
            | Action::OpenCandidate
            | Action::Quit
    );
    if state.focus == Focus::SessionTitle && leaves_title {
        commit_title_edit(state, effects);
    }
    if !matches!(action, Action::BeginPaneResize(_) | Action::DragPane { .. }) {
        state.dragging_divider = None;
    }
    let continuous = matches!(
        (&action, state.focus),
        (
            Action::Edit {
                target: EditorTarget::SessionTitle,
                command: EditCommand::Insert { typing: true, .. }
                    | EditCommand::Move {
                        cursor: CursorMove::Up { .. } | CursorMove::Down { .. },
                        ..
                    },
            },
            Focus::SessionTitle,
        ) | (
            Action::Edit {
                target: EditorTarget::Composer,
                command: EditCommand::Insert { typing: true, .. }
                    | EditCommand::Move {
                        cursor: CursorMove::Up { .. } | CursorMove::Down { .. },
                        ..
                    },
            },
            Focus::Composer,
        )
    );
    if !continuous {
        if state.focus == Focus::SessionTitle && state.title_edit.is_some() {
            state.title_editor_mut().break_interaction();
        } else {
            state.editor_mut().break_interaction();
        }
    }
    match action {
        Action::BeginPaneResize(divider) => state.dragging_divider = Some(divider),
        Action::DragPane { widths, finish } => {
            if state.dragging_divider.is_some() {
                state.pane_widths = widths;
                if finish {
                    state.dragging_divider = None;
                }
            }
        }
        Action::EndPaneResize => {}
        Action::Edit { target, command } => edit(state, target, command, effects),
        Action::CommitTitle => commit_title_edit(state, effects),
        Action::CancelTitle => {
            let baseline = state
                .title_edit
                .as_ref()
                .and_then(|edit| intended_session_title(state, edit.target));
            if let Some(edit) = &mut state.title_edit {
                let text = baseline.unwrap_or_else(|| edit.original.clone());
                edit.original = text.clone();
                let revision = edit.editor.revision().wrapping_add(1);
                let cursor = text.len();
                edit.editor.reset_external(text, cursor, revision);
            }
            state.status = None;
            state.set_focus(Focus::Composer);
        }
        Action::StartSlashCommand => {
            if state.panel.is_none()
                && state.draft().is_empty()
                && state
                    .selected_ui()
                    .is_none_or(|ui| ui.selected_answer.is_none())
            {
                set_action_focus(state, Focus::Composer, effects);
                state.editor_mut().apply(EditCommand::Insert {
                    text: "/".into(),
                    typing: true,
                });
            }
        }
        Action::OpenModels => panel::open_models(state, effects),
        Action::SelectObject(index) => panel::select_object(state, index, effects),
        Action::OpenHistory(sequence) => panel::open_history(state, sequence, effects),
        Action::OpenJob(job) => panel::open_job(state, job, effects),
        Action::PanelPrevious => panel::panel_previous(state),
        Action::PanelNext => panel::panel_next(state),
        Action::SelectModel(index) => panel::select_model(state, index, effects),
        Action::SetupText(value) => panel::setup_text(state, value),
        Action::SetupClear => panel::setup_clear(state),
        Action::SetupBackspace => panel::setup_backspace(state),
        Action::NextField | Action::PreviousField => {
            panel::move_setup_field(state, matches!(action, Action::NextField));
        }
        Action::SelectField(field) => panel::select_setup_field(state, field),
        Action::ChooseConnectionKind(index) => panel::choose_connection_kind(state, index),
        Action::SaveConnection => panel::save_connection(state, effects),
        Action::ActivatePanel => panel::activate(state, effects),
        Action::ScrollPanel { amount, max } => panel::scroll_reader(state, amount, max),

        Action::Focus(focus) => {
            set_action_focus(state, focus, effects);
        }
        Action::FocusLeft => match state.focus {
            Focus::SessionTitle | Focus::Composer => {
                set_action_focus(state, Focus::Sessions, effects)
            }
            Focus::RightRail => set_action_focus(state, state.last_center_focus(), effects),
            Focus::Sessions => {}
        },
        Action::FocusRight => match state.focus {
            Focus::Sessions => set_action_focus(state, state.last_center_focus(), effects),
            Focus::SessionTitle | Focus::Composer => {
                set_action_focus(state, Focus::RightRail, effects)
            }
            Focus::RightRail => {}
        },
        Action::FocusUp => {
            if state.focus == Focus::Composer {
                set_action_focus(state, Focus::SessionTitle, effects);
            }
        }
        Action::FocusDown => {
            if state.focus == Focus::SessionTitle {
                set_action_focus(state, Focus::Composer, effects);
            }
        }
        Action::SelectPrevious => move_session(state, -1),
        Action::SelectNext => move_session(state, 1),
        Action::ScrollSessions { start } => state.session_scroll = Some(start),
        Action::OpenCandidate => {
            if let Some(session) = state.session_candidate.or(state.selected).or_else(|| {
                state
                    .session_rows
                    .iter()
                    .find(|row| !row.info().archived)
                    .map(SessionNavRow::id)
            }) {
                select_session(state, session, effects);
            }
        }
        Action::SelectSession(session) => select_session(state, session, effects),
        Action::SelectSlashPrevious => move_slash(state, -1),
        Action::SelectSlashNext => move_slash(state, 1),
        Action::CompleteSlash => complete_slash(state),
        Action::ExecuteCommand(kind) => {
            if let Some(index) = state
                .slash_matches()
                .iter()
                .position(|command| command.kind == kind)
            {
                state.slash_selection = index;
                submit(state, effects);
            }
        }
        Action::Submit => submit(state, effects),
        Action::ClickSubmit => {
            set_action_focus(state, Focus::Composer, effects);
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
                    set_action_focus(state, Focus::Composer, effects);
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
            if panel::escape(state, effects) {
                return;
            }
            if state.focus == Focus::Sessions {
                state.session_candidate = state.selected;
                state.session_scroll = None;
                state.set_focus(state.last_center_focus());
                return;
            }
            if state.slash_palette_visible() {
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
        Action::ScrollUp(amount) => scroll_up(state, amount, effects),
        Action::ScrollDown(amount) => {
            if let Some(ui) = state.selected_ui_mut()
                && ui.transcript.scroll_down(amount)
            {
                effects.push(Effect::ReloadRecentHistory {
                    session: ui.id,
                    generation: ui.generation,
                });
            }
        }
        Action::Stop => effects.extend(stop_selected(state)),
        Action::Quit => {
            prepare_exit(state);
            dispatch_queued_titles_for_exit(state, effects);
            effects.push(Effect::Shutdown);
        }
    }
}

fn edit(
    state: &mut UiState,
    target: EditorTarget,
    mut command: EditCommand,
    effects: &mut Vec<Effect>,
) {
    if state.panel.is_some() {
        return;
    }
    let points = matches!(&command, EditCommand::Point { .. });
    let changes_text = matches!(
        &command,
        EditCommand::Insert { .. }
            | EditCommand::Replace { .. }
            | EditCommand::DeleteBefore
            | EditCommand::DeleteAfter
            | EditCommand::Clear
            | EditCommand::Undo
            | EditCommand::Redo
    );
    match target {
        EditorTarget::Composer => {
            if points {
                set_action_focus(state, Focus::Composer, effects);
            } else if state.focus != Focus::Composer {
                return;
            }
            state.editor_mut().apply(command);
            if changes_text {
                state.status = None;
                state.slash_selection = 0;
                state.slash_dismissed = None;
            }
        }
        EditorTarget::SessionTitle => {
            if points {
                set_action_focus(state, Focus::SessionTitle, effects);
            } else if state.focus != Focus::SessionTitle {
                return;
            }
            if !state.begin_title_edit() {
                return;
            }
            if let EditCommand::Insert { text, typing } = command {
                command = EditCommand::Insert {
                    text: text
                        .replace(['\r', '\n', '\t'], " ")
                        .chars()
                        .filter(|ch| !ch.is_control())
                        .collect(),
                    typing,
                };
            }
            state.title_editor_mut().apply(command);
            if changes_text {
                state.status = None;
            }
        }
    }
}

/// Moves focus for a user action and commits the title exactly when that move
/// leaves the title editor. Keeping this at the focus mutation point avoids
/// persisting a title for unrelated scrolling or divider gestures.
fn set_action_focus(state: &mut UiState, focus: Focus, effects: &mut Vec<Effect>) {
    if state.focus == Focus::SessionTitle && focus != Focus::SessionTitle {
        commit_title_edit(state, effects);
    }
    if focus != Focus::SessionTitle || state.begin_title_edit() {
        state.set_focus(focus);
    }
}

fn prepare_exit(state: &mut UiState) {
    for ui in state.session_ui.values_mut() {
        for (_, answer) in std::mem::take(&mut ui.answer_drafts) {
            if !answer.editor.is_empty() {
                let restored = answer::append_restored_text(
                    ui.draft.text(),
                    &format!("[Unsent answer / 未发送回答]\n{}", answer.editor.text()),
                );
                let cursor = restored.len();
                ui.draft.replace_user(restored, cursor);
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
            Ok(input) => (input, answer.editor.revision()),
            Err(_) => {
                state.status = Some("This question has ended. Your answer is preserved; convert it explicitly to send ordinary text".into());
                return;
            }
        }
    } else {
        (SubmitInput::new(text.clone()), ui.draft.revision())
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
        session: ui.id,
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
            revision: state.orphan_draft.revision(),
            text: text.clone(),
        },
        first_input: Some(PendingSubmission {
            request_id: RequestId::new(),
            text,
            draft_revision: state.orphan_draft.revision(),
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
        let restored = answer::append_restored_text(ui.draft.text(), answer.editor.text());
        let cursor = restored.len();
        ui.draft.replace_user(restored, cursor);
        ui.selected_answer = None;
        state.status = Some("Answer copied to ordinary draft; review before sending".into());
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
    let candidate = answer::recoverable_inputs(snapshot, ui.transcript.entries())
        .into_iter()
        .find(|candidate| match candidate {
            RecoveryCandidate::Retry { input: id }
            | RecoveryCandidate::Restore { input: id, .. } => *id == input,
        });
    match candidate {
        Some(RecoveryCandidate::Retry { input }) if retry => {
            effects.push(Effect::RetryInput {
                session: ui.id,
                generation: ui.generation,
                input,
            });
            state.status = Some("Retry requested for the saved input".into());
        }
        Some(RecoveryCandidate::Restore { text, reply_to, .. }) if !retry => {
            set_action_focus(state, Focus::Composer, effects);
            let Some(ui) = state.selected_ui_mut() else {
                return;
            };
            if let Some(question) = reply_to {
                let answer = ui
                    .answer_drafts
                    .entry(question)
                    .or_insert_with(|| AnswerDraft::new(question));
                let restored = answer::append_restored_text(answer.editor.text(), &text);
                answer.replace(restored, usize::MAX);
                ui.selected_answer = Some(question);
                state.status = Some("Answer restored with its original question; an expired answer cannot be sent without explicit conversion".into());
            } else {
                let restored = answer::append_restored_text(ui.draft.text(), &text);
                let cursor = restored.len();
                ui.draft.replace_user(restored, cursor);
                ui.selected_answer = None;
                state.status = Some("Input restored to your draft; review before sending".into());
            }
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
        session: ui.id,
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
                    super::answer::recoverable_inputs(snapshot, ui.transcript.entries())
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
                        super::answer::recoverable_inputs(snapshot, ui.transcript.entries())
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
                panel::open_models(state, effects);
            } else if parts.len() == 2 {
                let profile = parts[0].to_owned();
                let model = parts[1].to_owned();
                clear_current_draft(state, effects);
                panel::set_named_model(state, profile, model, effects);
            } else {
                state.status = Some("Usage: /model [profile model]".into());
            }
        }
        CommandKind::Details if argument.is_empty() => {
            clear_current_draft(state, effects);
            panel::open_objects(state, effects);
        }

        CommandKind::New => {
            if let Some(pending) = &state.pending_create {
                let retry = pending.failed
                    && (argument == "--retry" || create_source_matches(state, &pending.source));
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
            let source = state.selected_ui().map_or_else(
                || DraftSource::Orphan {
                    revision: state.orphan_draft.revision(),
                    text: state.orphan_draft.text().to_owned(),
                },
                |ui| DraftSource::Session {
                    id: ui.id,
                    generation: ui.generation,
                    revision: ui.draft.revision(),
                    text: ui.draft.text().to_owned(),
                },
            );
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
            state.set_focus(Focus::Sessions);
            clear_current_draft(state, effects);
        }
        CommandKind::Rename if argument.is_empty() => {
            if state.selected_ui().is_some() {
                clear_current_draft(state, effects);
                state.begin_title_edit();
                state.set_focus(Focus::SessionTitle);
                state.caret_visible = true;
            } else {
                state.status = Some("There is no session to rename".into());
            }
        }
        CommandKind::Rename if !argument.is_empty() => {
            if state.begin_title_edit() {
                clear_current_draft(state, effects);
                if let Some(edit) = &mut state.title_edit {
                    edit.editor.replace_user(argument.into(), argument.len());
                }
                commit_title_edit(state, effects);
            } else {
                state.status = Some("There is no session to rename".into());
            }
        }
        CommandKind::Help if argument.is_empty() => {
            clear_current_draft(state, effects);
            panel::open_help(state, effects);
        }
        CommandKind::Quit if argument.is_empty() => {
            clear_current_draft(state, effects);
            prepare_exit(state);
            dispatch_queued_titles_for_exit(state, effects);
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
                && state.orphan_draft.revision() == *revision
                && state.orphan_draft.text() == *text
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
                        && ui.draft.revision() == *revision
                        && ui.draft.text() == *text
                })
        }
    }
}

fn clear_current_draft(state: &mut UiState, effects: &mut Vec<Effect>) {
    if let Some(ui) = state.selected_ui_mut() {
        ui.draft.clear();
        effects.push(Effect::SaveDraft {
            session: ui.id,
            generation: ui.generation,
            revision: ui.draft.revision(),
            text: String::new(),
        });
    } else {
        state.orphan_draft.clear();
    }
    state.slash_selection = 0;
    state.slash_dismissed = None;
}

fn clear_create_source(state: &mut UiState, source: DraftSource, effects: &mut Vec<Effect>) {
    match source {
        DraftSource::None => {}
        DraftSource::Orphan { revision, text }
            if state.orphan_draft.revision() == revision && state.orphan_draft.text() == text =>
        {
            state.orphan_draft.clear();
        }
        DraftSource::Session {
            id,
            generation,
            revision,
            text,
        } => {
            if let Some(ui) = state.session_ui.get_mut(&id)
                && ui.generation == generation
                && ui.draft.revision() == revision
                && ui.draft.text() == text
            {
                ui.draft.clear();
                effects.push(Effect::SaveDraft {
                    session: id,
                    generation,
                    revision: ui.draft.revision(),
                    text: String::new(),
                });
            }
        }
        DraftSource::Orphan { .. } => {}
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
            ui.draft.apply(EditCommand::Replace { text: replacement });
        } else {
            state
                .orphan_draft
                .apply(EditCommand::Replace { text: replacement });
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
        .session_rows
        .iter()
        .filter(|row| !row.info().archived)
        .map(SessionNavRow::id)
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
    let Some(row) = state.session_row(id).filter(|row| !row.info().archived) else {
        return;
    };
    let id = row.id();
    state.session_candidate = Some(id);
    state.session_scroll = None;
    if state.selected == Some(id) {
        return;
    }
    panel::dismiss(state, effects);
    if let Some(previous) = state.selected
        && let Some(ui) = state.session_ui.get(&previous)
        && ui.draft.revision() <= ui.saved_draft_revision
        && ui.submitting.is_none()
        && !state.title_rename_pending(previous)
    {
        effects.push(Effect::ReleaseSession {
            session: previous,
            generation: ui.generation,
        });
    }
    state.selected = Some(id);
    clear_title_rename_error_for_other_session(state, id);
    panel::refresh_model_label(state, effects);
    state.slash_dismissed = None;
    let generation = state.generation();
    let ui = state
        .session_ui
        .entry(id)
        .or_insert_with(|| SessionUi::new(id, generation));
    ui.generation = generation;
    ui.transcript.start_generation();
    effects.push(Effect::OpenSession {
        session: id,
        generation,
    });
    if state.workspace_label.is_some() {
        effects.push(Effect::RememberSession { session: id });
    }
    // Opening another Session changes content, not the user's chosen region.
    // If the title already owned focus, prepare the new title editor without
    // moving focus there from any other region.
    if state.focus == Focus::SessionTitle {
        state.begin_title_edit();
    }
}

fn scroll_up(state: &mut UiState, amount: usize, effects: &mut Vec<Effect>) {
    if let Some(ui) = state.selected_ui_mut()
        && let Some(cursor) = ui.transcript.scroll_up(amount)
    {
        effects.push(Effect::LoadOlderHistory {
            session: ui.id,
            generation: ui.generation,
            cursor,
        });
    }
}

fn stop_selected(state: &UiState) -> Vec<Effect> {
    state
        .selected_ui()
        .filter(|ui| ui.working())
        .map(|ui| Effect::Stop {
            session: ui.id,
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
        ui.transcript.retain_metrics(metrics, limit);
    }
    trim_history_to_limit(state, limit);
    state
        .selected_ui()
        .is_some_and(|ui| ui.transcript.has_metrics())
}

fn retained_history_bytes(state: &UiState) -> usize {
    state
        .session_ui
        .values()
        .map(|ui| ui.transcript.allocated_bytes())
        .sum()
}

fn trim_history_to_limit(state: &mut UiState, limit: usize) {
    for (id, ui) in &mut state.session_ui {
        if Some(*id) != state.selected {
            ui.transcript.clear_layouts();
        }
    }
    let mut total = retained_history_bytes(state);
    // Derived layouts are expendable. Never evict source history merely because
    // an old or individually oversized layout cannot fit the cache budget.
    if total > limit {
        if let Some(ui) = state.selected_ui_mut() {
            ui.transcript.clear_older_layout();
        }
        total = retained_history_bytes(state);
    }
    if total > limit {
        if let Some(ui) = state.selected_ui_mut() {
            ui.transcript.clear_current_layout();
        }
        total = retained_history_bytes(state);
    }
    while total > limit {
        let victim = state
            .session_ui
            .iter()
            .find(|(id, ui)| Some(**id) != state.selected && ui.transcript.has_entries())
            .map(|(id, _)| *id)
            .or_else(|| {
                state.selected.filter(|id| {
                    state
                        .session_ui
                        .get(id)
                        .is_some_and(|ui| ui.transcript.has_entries())
                })
            });
        let Some(victim) = victim else {
            break;
        };
        let ui = state
            .session_ui
            .get_mut(&victim)
            .expect("selected cache victim exists");
        let Some(bytes) = ui.transcript.evict_one() else {
            break;
        };
        total = total.saturating_sub(bytes);
    }
}

#[cfg(test)]
fn test_editor(command: EditCommand) -> Action {
    Action::Edit {
        target: EditorTarget::Composer,
        command,
    }
}

#[cfg(test)]
fn test_title_editor(command: EditCommand) -> Action {
    Action::Edit {
        target: EditorTarget::SessionTitle,
        command,
    }
}

#[cfg(test)]
fn test_insert(text: impl Into<String>) -> Action {
    test_editor(EditCommand::Insert {
        text: text.into(),
        typing: false,
    })
}

#[cfg(test)]
fn test_title_insert(text: impl Into<String>) -> Action {
    test_title_editor(EditCommand::Insert {
        text: text.into(),
        typing: false,
    })
}

#[cfg(test)]
fn test_type(character: char) -> Action {
    test_editor(EditCommand::Insert {
        text: character.into(),
        typing: true,
    })
}

#[cfg(test)]
fn test_session_row(info: bone_app::SessionInfo) -> SessionNavRow {
    SessionNavRow::provisional(info)
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
        update(&mut state, UiEvent::Action(test_insert("/does-not-exist")));
        let effects = update(&mut state, UiEvent::Action(Action::Submit));
        assert!(effects.is_empty());
        assert_eq!(state.orphan_draft.text(), "/does-not-exist");
    }

    #[test]
    fn command_hint_starts_slash_only_for_an_empty_composer() {
        let mut state = UiState::default();
        state.set_focus(Focus::Sessions);
        update(&mut state, UiEvent::Action(Action::StartSlashCommand));
        assert_eq!(state.draft(), "/");
        assert_eq!(state.focus, Focus::Composer);
        assert!(state.slash_palette_visible());

        update(&mut state, UiEvent::Action(test_insert("keep")));
        let draft = state.draft().to_owned();
        update(&mut state, UiEvent::Action(Action::StartSlashCommand));
        assert_eq!(state.draft(), draft);
    }

    #[test]
    fn orphan_text_is_kept_until_new_session_exists() {
        let mut state = UiState::default();
        update(&mut state, UiEvent::Action(test_insert("keep me")));
        let effects = update(&mut state, UiEvent::Action(Action::Submit));
        assert!(matches!(effects.as_slice(), [Effect::CreateSession { .. }]));
        assert_eq!(state.orphan_draft.text(), "keep me");
    }
}

#[cfg(test)]
mod owned_editor_lifecycle_tests {
    use super::*;

    fn info(title: &str) -> bone_app::SessionInfo {
        bone_app::SessionInfo {
            id: SessionId::new(),
            workspace: bone_app::WorkspaceId::new(),
            title: title.into(),
            archived: false,
        }
    }

    fn snapshot(
        info: &bone_app::SessionInfo,
        draft: &str,
    ) -> std::sync::Arc<bone_app::SessionView> {
        std::sync::Arc::new(bone_app::SessionView {
            session: info.clone(),
            runtime: bone_app::RuntimeState::Detached,
            draft: draft.into(),
            inputs: Vec::new(),
            jobs: Vec::new(),
            activity: Vec::new(),
            history_through: bone_app::SessionSeq(0),
            problem: None,
        })
    }

    fn opened(state: &mut UiState, info: &bone_app::SessionInfo, generation: u64, draft: &str) {
        update(
            state,
            UiEvent::SessionOpened {
                session: info.id,
                generation,
                snapshot: snapshot(info, draft),
                history: bone_app::RecentHistoryPage {
                    items: Vec::new(),
                    older_cursor: None,
                    snapshot_through: bone_app::SessionSeq(0),
                },
            },
        );
    }

    #[test]
    fn hydration_reconcile_cannot_undo_away_the_persisted_prefix() {
        let info = info("hydrate");
        let mut ui = SessionUi::new(info.id, 1);
        ui.draft.apply(EditCommand::Replace {
            text: "local".into(),
        });
        let mut state = UiState::default();
        state.session_rows.push(test_session_row(info.clone()));
        state.selected = Some(info.id);
        state.session_ui.insert(info.id, ui);

        opened(&mut state, &info, 1, "persisted");
        assert_eq!(state.draft(), "persisted\nlocal");
        let revision = state.editor().revision();
        update(&mut state, UiEvent::Action(test_editor(EditCommand::Undo)));
        assert_eq!(state.draft(), "persisted\nlocal");
        assert_eq!(state.editor().revision(), revision);
    }

    #[test]
    fn reopening_an_empty_saved_buffer_keeps_revision_identity() {
        let first = info("first");
        let second = info("second");
        let mut first_ui = SessionUi::new(first.id, 1);
        first_ui.draft.reset_external(String::new(), 0, 7);
        first_ui.saved_draft_revision = 7;
        let mut state = UiState::default();
        state.session_rows = vec![
            test_session_row(first.clone()),
            test_session_row(second.clone()),
        ];
        state.selected = Some(first.id);
        state.session_ui.insert(first.id, first_ui);
        state
            .session_ui
            .insert(second.id, SessionUi::new(second.id, 1));

        opened(&mut state, &first, 1, "");
        assert_eq!(state.editor().revision(), 7);
        update(&mut state, UiEvent::Action(test_insert("x")));
        assert_eq!(state.editor().revision(), 8);
        let saved = update(&mut state, UiEvent::PersistDraftsRequested);
        assert!(matches!(
            saved.as_slice(),
            [Effect::SaveDraft { session, revision: 8, text, .. }]
                if *session == first.id && text == "x"
        ));
        update(
            &mut state,
            UiEvent::DraftSaved {
                session: first.id,
                generation: 1,
                revision: 8,
            },
        );
        let effects = update(
            &mut state,
            UiEvent::Action(Action::SelectSession(second.id)),
        );
        assert!(effects.iter().any(
            |effect| matches!(effect, Effect::ReleaseSession { session, .. } if *session == first.id)
        ));
    }

    #[test]
    fn same_text_slash_completion_is_a_whole_document_edit_boundary() {
        let mut state = UiState::default();
        update(&mut state, UiEvent::Action(test_insert("/help")));
        let end = state.draft().len();
        update(
            &mut state,
            UiEvent::Action(test_editor(EditCommand::Point {
                byte: end,
                extend: false,
            })),
        );
        update(
            &mut state,
            UiEvent::Action(test_editor(EditCommand::Point {
                byte: 0,
                extend: true,
            })),
        );
        let revision = state.editor().revision();

        update(&mut state, UiEvent::Action(Action::CompleteSlash));
        assert_eq!(state.draft(), "/help");
        assert_eq!(state.draft_cursor(), state.draft().len());
        assert!(state.editor().selection().is_none());
        assert_eq!(state.editor().revision(), revision.wrapping_add(1));

        update(&mut state, UiEvent::Action(test_editor(EditCommand::Undo)));
        assert_eq!(state.draft(), "/help");
        assert_eq!(state.draft_cursor(), 0);
    }
}

#[cfg(test)]
mod async_identity_tests {
    use super::*;

    #[test]
    fn switching_sessions_clears_label_and_rejects_late_previous_selection() {
        let mut state = UiState::default();
        let workspace = bone_app::WorkspaceId::new();
        let a = SessionId::new();
        let b = SessionId::new();
        state.session_rows = [a, b]
            .into_iter()
            .map(|id| bone_app::SessionInfo {
                id,
                workspace,
                title: "test".into(),
                archived: false,
            })
            .map(test_session_row)
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
}

fn commit_title_edit(state: &mut UiState, effects: &mut Vec<Effect>) {
    let Some(target) = state.title_edit.as_ref().map(|edit| edit.target) else {
        return;
    };
    if !state.session_ui.contains_key(&target) {
        return;
    }

    let fallback = intended_session_title(state, target);
    let title = {
        let edit = state
            .title_edit
            .as_mut()
            .expect("title target was read above");
        let title = edit.editor.text().trim().to_owned();
        if title.is_empty() || title.len() > 200 {
            let text = fallback.unwrap_or_else(|| edit.original.clone());
            let revision = edit.editor.revision().wrapping_add(1);
            let cursor = text.len();
            edit.editor.reset_external(text, cursor, revision);
            state.status = Some("Use a nonempty title of at most 200 bytes".into());
            return;
        }
        if edit.editor.text() != title {
            let revision = edit.editor.revision().wrapping_add(1);
            edit.editor
                .reset_external(title.clone(), title.len(), revision);
        }
        title
    };
    let committed = committed_session_title(state, target).unwrap_or_default();
    let queue = state.title_renames.entry(target).or_default();
    let current = queue
        .pending
        .last_key_value()
        .map_or(committed.as_str(), |(_, pending)| pending.as_str());
    let changed = current != title;
    queue.queued = changed.then_some(title);
    if changed {
        state.title_manual_intent.insert(target);
    }
    clear_title_rename_error(state, target);
    dispatch_queued_title(state, target, effects);
    state.status = None;
}

fn dispatch_queued_title(state: &mut UiState, session: SessionId, effects: &mut Vec<Effect>) {
    let title = {
        let Some(queue) = state.title_renames.get_mut(&session) else {
            return;
        };
        if !queue.pending.is_empty() {
            return;
        }
        queue.queued.take()
    };
    let Some(title) = title else {
        return;
    };
    if committed_session_title(state, session).as_deref() == Some(title.as_str()) {
        return;
    }
    enqueue_title_rename(state, session, title, effects);
}

fn dispatch_queued_titles_for_exit(state: &mut UiState, effects: &mut Vec<Effect>) {
    let sessions = state
        .title_renames
        .iter()
        .filter_map(|(session, queue)| queue.queued.as_ref().map(|_| *session))
        .collect::<Vec<_>>();
    for session in sessions {
        let title = state
            .title_renames
            .get_mut(&session)
            .and_then(|queue| queue.queued.take())
            .expect("only queues with a title were collected");
        let already_last = state
            .title_renames
            .get(&session)
            .and_then(|queue| queue.pending.last_key_value())
            .is_some_and(|(_, pending)| pending == &title);
        if !already_last {
            enqueue_title_rename(state, session, title, effects);
        }
    }
}

fn enqueue_title_rename(
    state: &mut UiState,
    session: SessionId,
    title: String,
    effects: &mut Vec<Effect>,
) {
    let request = state.generation();
    state
        .title_renames
        .entry(session)
        .or_default()
        .pending
        .insert(request, title.clone());
    effects.push(Effect::RenameSession {
        session,
        request,
        title,
    });
}

fn committed_session_title(state: &UiState, session: SessionId) -> Option<String> {
    state
        .title_renames
        .get(&session)
        .and_then(|queue| queue.confirmed.clone())
        .or_else(|| {
            state
                .session_row(session)
                .map(|row| row.info().title.clone())
        })
}

fn intended_session_title(state: &UiState, session: SessionId) -> Option<String> {
    state
        .title_renames
        .get(&session)
        .and_then(TitleRenameQueue::desired)
        .map(str::to_owned)
        .or_else(|| committed_session_title(state, session))
}

fn set_committed_session_title(state: &mut UiState, session: SessionId, title: String) {
    if let Some(row) = state.session_row_mut(session) {
        row.summary.session.title = title;
    }
}

fn reconcile_committed_title(
    state: &mut UiState,
    session: SessionId,
    authoritative: &str,
    current: Option<&str>,
) -> (String, bool) {
    let (confirmation_observed, pending, confirmed) =
        state
            .title_renames
            .get_mut(&session)
            .map_or((false, false, None), |queue| {
                let confirmation_observed = queue.confirmed.as_deref() == Some(authoritative);
                if confirmation_observed {
                    queue.confirmed = None;
                }
                (
                    confirmation_observed,
                    queue.pending(),
                    queue.confirmed.clone(),
                )
            });

    let reconciled = if confirmation_observed {
        (authoritative.to_owned(), !pending)
    } else if let Some(confirmed) = confirmed {
        (confirmed, false)
    } else if pending {
        (current.unwrap_or(authoritative).to_owned(), false)
    } else {
        (authoritative.to_owned(), true)
    };

    if state
        .title_renames
        .get(&session)
        .is_some_and(|queue| !queue.pending() && queue.confirmed.is_none())
    {
        state.title_renames.remove(&session);
    }
    reconciled
}

fn merge_authoritative_session_info(
    state: &mut UiState,
    session: SessionId,
    authoritative: &bone_app::SessionInfo,
) {
    let Some(current) = state
        .session_row(session)
        .map(|row| row.info().title.clone())
    else {
        return;
    };
    let (title, update_editor) =
        reconcile_committed_title(state, session, &authoritative.title, Some(&current));
    let mut info = authoritative.clone();
    info.title = title;
    state
        .session_row_mut(session)
        .expect("the Session row was read above")
        .summary
        .session = info;
    if update_editor {
        update_title_editor_baseline(state, session, &authoritative.title);
    }
}

fn update_title_editor_baseline(state: &mut UiState, session: SessionId, authoritative: &str) {
    if let Some(edit) = state
        .title_edit
        .as_mut()
        .filter(|edit| edit.target == session)
    {
        let clean = edit.editor.text() == edit.original;
        edit.original = authoritative.to_owned();
        if clean {
            let revision = edit.editor.revision();
            let cursor = edit.original.len();
            edit.editor
                .reset_external(edit.original.clone(), cursor, revision);
        }
    }
}

fn reconcile_overview_rows(state: &mut UiState, rows: &mut [SessionNavRow]) {
    for row in rows {
        let session = row.id();
        let authoritative = row.info().title.clone();
        let current = state
            .session_row(session)
            .map(|current| current.info().title.clone());
        let (title, update_editor) =
            reconcile_committed_title(state, session, &authoritative, current.as_deref());
        row.summary.session.title = title;
        if update_editor {
            update_title_editor_baseline(state, session, &authoritative);
        }
    }
}

fn set_title_rename_error(state: &mut UiState, session: SessionId, message: String) {
    state.status = Some(message.clone());
    state.title_rename_error = Some((session, message));
}

fn clear_title_rename_error(state: &mut UiState, session: SessionId) {
    let Some((owner, message)) = state.title_rename_error.as_ref() else {
        return;
    };
    if *owner != session {
        return;
    }
    if state.status.as_deref() == Some(message.as_str()) {
        state.status = None;
    }
    state.title_rename_error = None;
}

fn clear_title_rename_error_for_other_session(state: &mut UiState, selected: SessionId) {
    if state
        .title_rename_error
        .as_ref()
        .is_some_and(|(owner, _)| *owner != selected)
    {
        let owner = state
            .title_rename_error
            .as_ref()
            .expect("rename error was checked above")
            .0;
        clear_title_rename_error(state, owner);
    }
}

fn finish_title_rename(state: &mut UiState, session: SessionId, effects: &mut Vec<Effect>) {
    if state.title_rename_pending(session) {
        return;
    }
    if state
        .title_renames
        .get(&session)
        .is_some_and(|queue| queue.confirmed.is_none())
    {
        state.title_renames.remove(&session);
    }
    if !state.quitting {
        release_inactive_session(state, session, effects);
    }
}

fn release_inactive_session(state: &UiState, session: SessionId, effects: &mut Vec<Effect>) {
    if state.selected == Some(session) || state.title_rename_pending(session) {
        return;
    }
    let Some(ui) = state.session_ui.get(&session) else {
        return;
    };
    if ui.draft.revision() <= ui.saved_draft_revision
        && ui.submitting.is_none()
        && !effects.iter().any(
            |effect| matches!(effect, Effect::ReleaseSession { session: queued, .. } if *queued == session),
        )
    {
        effects.push(Effect::ReleaseSession {
            session,
            generation: ui.generation,
        });
    }
}

#[cfg(test)]
mod panel_draft_tests {
    use super::*;
    use bone_app::{
        HistoryEntry, HistoryPage, InputId, JobRef, OutcomeKind, QuestionId, RecentHistoryPage,
        RuntimeId, SessionEvent, SessionInfo, SessionSeq, WorkspaceId,
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
        let mut ui = SessionUi::new(info.id, 1);
        ui.draft.reset_external("ordinary draft".into(), 4, 8);
        let mut answer = AnswerDraft::new(question);
        answer.replace("answer draft".into(), 3);
        ui.answer_drafts.insert(question, answer);
        ui.selected_answer = Some(question);
        ui.transcript.open(RecentHistoryPage {
            items: vec![HistoryEntry {
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
            }],
            older_cursor: None,
            snapshot_through: SessionSeq(1),
        });
        let mut state = UiState::default();
        state.session_rows.push(test_session_row(info));
        state.selected = Some(id);
        state.session_ui.insert(id, ui);
        (state, question)
    }

    #[test]
    fn clear_answer_preserves_ordinary_draft_and_question_binding() {
        let (mut state, question) = fixture();
        let revision = state.selected_ui().unwrap().answer_drafts[&question].revision();
        assert!(update(&mut state, UiEvent::Action(test_editor(EditCommand::Clear))).is_empty());
        assert_eq!(state.draft(), "");
        let ui = state.selected_ui().unwrap();
        assert_eq!(ui.draft(), "ordinary draft");
        assert_eq!(ui.selected_answer, Some(question));
        assert!(ui.answer_drafts[&question].revision() > revision);
        update(&mut state, UiEvent::Action(test_editor(EditCommand::Undo)));
        assert_eq!(state.draft(), "answer draft");
        assert_eq!(state.selected_ui().unwrap().draft(), "ordinary draft");
    }

    #[test]
    fn editor_history_isolated_between_orphan_session_and_answer() {
        let (mut state, question) = fixture();
        let session = state.selected;
        state.selected = None;
        update(&mut state, UiEvent::Action(test_insert("orphan")));
        state.selected = session;
        update(&mut state, UiEvent::Action(test_insert("ANSWER")));
        state.selected_ui_mut().unwrap().selected_answer = None;
        update(&mut state, UiEvent::Action(test_insert("SESSION")));
        update(&mut state, UiEvent::Action(test_editor(EditCommand::Undo)));
        assert_eq!(state.draft(), "ordinary draft");
        state.selected_ui_mut().unwrap().selected_answer = Some(question);
        assert!(state.draft().contains("ANSWER"));
        update(&mut state, UiEvent::Action(test_editor(EditCommand::Undo)));
        assert_eq!(state.draft(), "answer draft");
        state.selected = None;
        assert_eq!(state.draft(), "orphan");
        update(&mut state, UiEvent::Action(test_editor(EditCommand::Undo)));
        assert_eq!(state.draft(), "");
        update(&mut state, UiEvent::Action(test_editor(EditCommand::Redo)));
        assert_eq!(state.draft(), "orphan");
    }

    #[test]
    fn unicode_selection_replacement_and_coalesced_typing_are_undoable() {
        let mut state = UiState::default();
        for ch in "中e\u{301}🙂".chars() {
            update(&mut state, UiEvent::Action(test_type(ch)));
        }
        update(&mut state, UiEvent::Action(test_editor(EditCommand::Undo)));
        assert_eq!(state.draft(), "");
        update(&mut state, UiEvent::Action(test_editor(EditCommand::Redo)));
        for _ in 0..2 {
            update(
                &mut state,
                UiEvent::Action(test_editor(EditCommand::Move {
                    cursor: CursorMove::Left,
                    select: true,
                })),
            );
        }
        assert_eq!(state.editor().selection(), Some(3.."中e\u{301}🙂".len()));
        update(&mut state, UiEvent::Action(test_insert("字")));
        assert_eq!(state.draft(), "中字");
        update(&mut state, UiEvent::Action(test_editor(EditCommand::Undo)));
        assert_eq!(state.draft(), "中e\u{301}🙂");
    }

    #[test]
    fn undo_back_to_submitted_text_does_not_allow_stale_receipt_to_clear_it() {
        let (mut state, question) = fixture();
        for answer in [false, true] {
            let ui = state.selected_ui_mut().unwrap();
            ui.selected_answer = answer.then_some(question);
            let text = if answer {
                ui.answer_drafts[&question].text().to_owned()
            } else {
                ui.draft.text().to_owned()
            };
            let revision = if answer {
                ui.answer_drafts[&question].revision()
            } else {
                ui.draft.revision()
            };
            let request_id = RequestId::new();
            ui.submitting = Some(PendingSubmission {
                reply_to: answer.then_some(question),
                request_id,
                text: text.clone(),
                draft_revision: revision,
                failed: false,
            });
            let session = ui.id;
            update(&mut state, UiEvent::Action(test_insert("later")));
            update(&mut state, UiEvent::Action(test_editor(EditCommand::Undo)));
            assert_eq!(state.draft(), text);
            update(
                &mut state,
                UiEvent::Submitted {
                    session,
                    request_id,
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
            answer::append_restored_text(answer.text(), "restored"),
            usize::MAX,
        );
        update(&mut state, UiEvent::Action(test_editor(EditCommand::Undo)));
        assert_eq!(state.draft(), "answer draft");
        assert_eq!(state.selected_ui().unwrap().draft(), "ordinary draft");
    }

    #[test]
    fn editor_history_budget_is_global_and_prefers_current_buffer() {
        let (mut state, question) = fixture();
        let text = "x".repeat(1024 * 1024);
        state.orphan_draft = crate::editor::EditorBuffer::new(text.clone());
        for _ in 0..4 {
            state.orphan_draft.checkpoint();
        }
        let ui = state.selected_ui_mut().unwrap();
        ui.draft = crate::editor::EditorBuffer::new(text.clone());
        for _ in 0..4 {
            ui.draft.checkpoint();
        }
        ui.answer_drafts.get_mut(&question).unwrap().editor =
            crate::editor::EditorBuffer::new(text);
        for _ in 0..4 {
            ui.answer_drafts
                .get_mut(&question)
                .unwrap()
                .editor
                .checkpoint();
        }
        trim_editor_history(&mut state);
        let ui = state.selected_ui().unwrap();
        let total = state.orphan_draft.history_bytes()
            + ui.draft.history_bytes()
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
        let info = state
            .session_row(state.selected.expect("selected Session"))
            .expect("selected Session row")
            .info()
            .clone();
        let ui = state.selected_ui_mut().unwrap();
        ui.transcript.history_loaded(HistoryPage {
            items: vec![HistoryEntry {
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
            }],
            next_cursor: SessionSeq(2),
            has_more: false,
        });
        ui.snapshot = Some(std::sync::Arc::new(bone_app::SessionView {
            session: info,
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
        panel::open_objects(&mut state, &mut Vec::new());
        let Some(Panel::Objects(objects)) = &state.panel else {
            panic!("object menu")
        };
        let choices = &objects.choices;
        assert_eq!(choices.len(), 3);
        assert!(choices.iter().map(|(_, label)| label.len()).sum::<usize>() < 300);
        let target = choices
            .iter()
            .position(|(source, _)| *source == ReaderSource::History(SessionSeq(1)))
            .unwrap();
        update(&mut state, UiEvent::Action(Action::SelectObject(target)));
        assert!(
            matches!(&state.panel, Some(Panel::Reader(reader)) if reader.content.source == ReaderSource::History(SessionSeq(1)) && reader.content.text.contains("Done"))
        );
        assert_drafts(&state, question);
        update(&mut state, UiEvent::Action(Action::Escape));
        panel::open_objects(&mut state, &mut Vec::new());
        update(&mut state, UiEvent::Action(Action::ActivatePanel));
        assert!(
            matches!(&state.panel, Some(Panel::Reader(reader)) if reader.content.source == ReaderSource::Job(job))
        );
        assert_drafts(&state, question);
    }

    #[test]
    fn expired_or_cross_session_object_menu_never_substitutes_another_object() {
        let (mut state, question) = fixture();
        panel::open_objects(&mut state, &mut Vec::new());
        state
            .selected_ui_mut()
            .unwrap()
            .transcript
            .open(RecentHistoryPage {
                items: vec![],
                older_cursor: None,
                snapshot_through: SessionSeq(0),
            });
        update(&mut state, UiEvent::Action(Action::ActivatePanel));
        assert!(matches!(state.panel, Some(Panel::Objects(_))));
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
        assert!(matches!(state.panel, Some(Panel::Objects(_))));
        assert!(state.status.as_deref().unwrap().contains("session changed"));
    }

    fn assert_drafts(state: &UiState, question: QuestionId) {
        let ui = state.selected_ui().unwrap();
        assert_eq!(
            (ui.draft.text(), ui.draft.cursor(), ui.draft.revision()),
            ("ordinary draft", 4, 8)
        );
        assert_eq!(ui.selected_answer, Some(question));
        let answer = &ui.answer_drafts[&question];
        assert_eq!(
            (answer.text(), answer.cursor(), answer.revision()),
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
            session: state.session_row(session).unwrap().info().clone(),
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
        state.panel = Some(Panel::Reader(ReaderPanel {
            content: super::super::reader::ReaderContent::from_job(&snapshot, job).unwrap(),
            scroll: 8,
        }));
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
            matches!(&state.panel, Some(Panel::Reader(reader)) if reader.content.text.contains("Running"))
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
            matches!(&state.panel, Some(Panel::Reader(reader)) if reader.content.text.contains("New result"))
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
            matches!(&state.panel, Some(Panel::Reader(reader)) if reader.content.source == super::super::reader::ReaderSource::Job(job) && reader.content.text.contains("no longer in the current snapshot") && !reader.content.text.contains("New result"))
        );
        assert!(matches!(&state.panel, Some(Panel::Reader(reader)) if reader.scroll == 8));
        assert_eq!(state.focus, focus);
        assert_drafts(&state, question);

        let original = super::super::reader::ReaderContent::from_history(
            session,
            state
                .selected_ui()
                .unwrap()
                .transcript
                .entries()
                .next()
                .unwrap(),
        )
        .unwrap();
        state.panel = Some(Panel::Reader(ReaderPanel {
            content: original.clone(),
            scroll: 0,
        }));
        update(
            &mut state,
            UiEvent::SessionChanged {
                session,
                generation: 1,
                snapshot: std::sync::Arc::new(snapshot),
            },
        );
        assert!(matches!(&state.panel, Some(Panel::Reader(reader)) if reader.content == original));
    }

    #[test]
    fn model_object_and_help_panels_preserve_both_drafts() {
        let (mut state, question) = fixture();
        for panel in ["model", "details", "help"] {
            let mut effects = Vec::new();
            match panel {
                "model" => panel::open_models(&mut state, &mut effects),
                "details" => panel::open_objects(&mut state, &mut effects),
                _ => panel::open_help(&mut state, &mut effects),
            }
            assert!(
                effects.iter().all(|effect| !matches!(
                    effect,
                    Effect::SaveDraft { .. } | Effect::Submit { .. }
                ))
            );
            assert!(state.panel.is_some());
            if panel == "model" {
                let request = state.model_operation.expect("model load").request;
                let session = state.selected;
                update(
                    &mut state,
                    UiEvent::ModelsLoaded {
                        session,
                        request,
                        choices: vec![],
                        profiles: vec![],
                    },
                );
                let add = 0;
                update(&mut state, UiEvent::Action(Action::SelectModel(add)));
                update(&mut state, UiEvent::Action(Action::ChooseConnectionKind(0)));
                update(&mut state, UiEvent::Action(Action::SetupClear));
                update(
                    &mut state,
                    UiEvent::Action(Action::SetupText("e\u{301}👩‍💻".to_owned().into())),
                );
                update(&mut state, UiEvent::Action(Action::SetupBackspace));
                assert!(matches!(
                    &state.panel,
                    Some(Panel::Models(ModelPanel {
                        screen: ModelScreen::Setup(form),
                        ..
                    })) if form.label == "e\u{301}"
                ));
                update(&mut state, UiEvent::Action(Action::SetupBackspace));
                assert!(matches!(
                    &state.panel,
                    Some(Panel::Models(ModelPanel {
                        screen: ModelScreen::Setup(form),
                        ..
                    })) if form.label.is_empty()
                ));
                update(&mut state, UiEvent::Action(Action::Escape));
                assert!(matches!(state.panel, Some(Panel::Models(_))));
            }
            update(&mut state, UiEvent::Action(Action::Escape));
            assert!(state.panel.is_none());
            assert_drafts(&state, question);
        }
    }

    #[test]
    fn inline_title_editor_serializes_renames_without_touching_composer_drafts() {
        let (mut state, question) = fixture();
        let target = state.selected_ui().unwrap().id;

        assert!(
            update(
                &mut state,
                UiEvent::Action(Action::Focus(Focus::SessionTitle))
            )
            .is_empty()
        );
        assert_eq!(state.focus, Focus::SessionTitle);
        {
            let edit = state.title_edit.as_mut().unwrap();
            edit.editor.apply(EditCommand::Replace {
                text: "  New title  ".into(),
            });
        }
        let effects = update(&mut state, UiEvent::Action(Action::CommitTitle));
        let [
            Effect::RenameSession {
                session,
                request,
                title,
            },
        ] = effects.as_slice()
        else {
            panic!("expected rename effect")
        };
        assert_eq!(*session, target);
        assert_eq!(*request, 1);
        assert_eq!(title, "New title");
        assert_drafts(&state, question);

        update(
            &mut state,
            UiEvent::Action(test_title_editor(EditCommand::Move {
                cursor: CursorMove::LineEnd,
                select: false,
            })),
        );
        update(&mut state, UiEvent::Action(test_title_insert(" v2")));
        assert!(update(&mut state, UiEvent::Action(Action::CommitTitle)).is_empty());
        assert_eq!(state.title_text(), Some("New title v2"));

        let effects = update(
            &mut state,
            UiEvent::SessionRenamed {
                session: *session,
                request: *request,
                title: title.clone(),
            },
        );
        let [
            Effect::RenameSession {
                request: second_request,
                title: second_title,
                ..
            },
        ] = effects.as_slice()
        else {
            panic!("queued title should start after the first write")
        };
        assert_eq!(*second_request, 2);
        assert_eq!(second_title, "New title v2");
        assert_eq!(state.title_text(), Some("New title v2"));

        update(
            &mut state,
            UiEvent::SessionRenameFailed {
                session: *session,
                request: *second_request,
                message: "Could not save title".into(),
            },
        );
        assert_eq!(state.status.as_deref(), Some("Could not save title"));
        assert_eq!(
            state
                .session_row(*session)
                .expect("renamed Session row")
                .info()
                .title,
            "New title"
        );
        assert_eq!(state.title_text(), Some("New title"));
        assert_drafts(&state, question);
    }
}

// Editing history has a separate 8 MiB global budget. Inactive buffers are
// trimmed first; current text and revisions are never part of eviction.
fn trim_editor_history(state: &mut UiState) {
    let selected = state.selected;
    let mut editors = Vec::new();
    for (id, ui) in &mut state.session_ui {
        editors.push((
            Some(*id) == selected && ui.selected_answer.is_none(),
            &mut ui.draft,
        ));
        for (question, answer) in &mut ui.answer_drafts {
            editors.push((
                Some(*id) == selected && ui.selected_answer == Some(*question),
                &mut answer.editor,
            ));
        }
    }
    editors.push((selected.is_none(), &mut state.orphan_draft));
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
        state.session_rows = ids
            .iter()
            .map(|id| bone_app::SessionInfo {
                id: *id,
                workspace,
                title: "test".into(),
                archived: false,
            })
            .map(test_session_row)
            .collect();
        let mut effects = Vec::new();
        select_session(&mut state, ids[0], &mut effects);
        state.set_focus(Focus::Sessions);
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
        assert_eq!(state.focus, Focus::Sessions);
        assert_eq!(state.session_ui[&ids[0]].draft(), "unsent original");
        update(&mut state, UiEvent::Action(Action::SelectSession(ids[2])));
        assert_eq!(state.selected, Some(ids[2]));
        assert_eq!(state.session_candidate, Some(ids[2]));
        assert_eq!(state.focus, Focus::Sessions);
    }

    #[test]
    fn session_switch_preserves_every_workspace_focus_region() {
        for focus in [
            Focus::Sessions,
            Focus::SessionTitle,
            Focus::Composer,
            Focus::RightRail,
        ] {
            let (mut state, ids) = fixture();
            if focus == Focus::SessionTitle {
                update(
                    &mut state,
                    UiEvent::Action(Action::Focus(Focus::SessionTitle)),
                );
            } else {
                state.set_focus(focus);
            }

            update(&mut state, UiEvent::Action(Action::SelectSession(ids[1])));

            assert_eq!(state.selected, Some(ids[1]));
            assert_eq!(state.focus, focus);
            if focus == Focus::SessionTitle {
                let ui = state.selected_ui().unwrap();
                assert_eq!(
                    state.title_edit.as_ref().map(|edit| edit.target),
                    Some(ui.id)
                );
            }
        }
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
        assert_eq!(state.focus, Focus::Composer);
        assert_eq!(state.single_pane(), crate::layout::SinglePane::Conversation);
        assert_eq!(state.session_scroll, None);
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
        state.session_rows.push(test_session_row(info.clone()));
        state.session_ui.insert(id, SessionUi::new(info.id, 1));
        id
    }

    #[test]
    fn pointer_submission_focuses_composer_but_reading_submission_does_not() {
        let mut state = UiState::default();
        state.orphan_draft = "send this".into();
        state.set_focus(Focus::SessionTitle);
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
        update(
            &mut state,
            UiEvent::SubmitFailed {
                session: background,
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
        assert_eq!(state.status.as_deref(), Some("current status"));
        update(
            &mut state,
            UiEvent::OperationFailed {
                kind: OperationKind::ReloadRecentHistory,
                session: Some(background),
                generation: Some(1),
                message: "background recent history failed".into(),
            },
        );
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

    fn metrics(sequences: impl IntoIterator<Item = u64>) -> Arc<TranscriptMetrics> {
        Arc::new(TranscriptMetrics {
            anchors: sequences
                .into_iter()
                .map(|sequence| ContentAnchor {
                    sequence: bone_app::SessionSeq(sequence),
                    byte: 0,
                    part: AnchorPart::Text,
                })
                .collect::<Vec<_>>()
                .into(),
            ..TranscriptMetrics::default()
        })
    }

    fn fixture() -> UiState {
        let mut state = UiState::default();
        let info = bone_app::SessionInfo {
            id: SessionId::new(),
            workspace: bone_app::WorkspaceId::new(),
            title: "budget".into(),
            archived: false,
        };
        let mut ui = SessionUi::new(info.id, 1);
        let entry = bone_app::HistoryEntry {
            sequence: bone_app::SessionSeq(1),
            occurred_at: 0,
            event: bone_app::SessionEvent::InputCancelled {
                input: bone_app::InputId(1),
            },
        };
        ui.transcript.open(bone_app::RecentHistoryPage {
            items: vec![entry],
            older_cursor: None,
            snapshot_through: bone_app::SessionSeq(1),
        });
        ui.transcript
            .retain_metrics(metrics([1]), HISTORY_CACHE_BYTES);
        ui.transcript.pin_reading();
        ui.transcript.clear_current_layout();
        state.selected = Some(info.id);
        state.session_ui.insert(info.id, ui);
        state
    }

    #[test]
    fn individually_oversized_metrics_are_rejected_without_evicting_history_or_anchor() {
        let mut state = fixture();
        let anchor = state.selected_ui().unwrap().transcript.read_anchor();
        let metrics = metrics(1..=8);
        let limit = metrics.allocated_bytes() - 1;
        assert!(state.selected_ui().unwrap().transcript.allocated_bytes() < limit);
        assert!(!retain_transcript_with_limit(&mut state, metrics, limit));
        let ui = state.selected_ui().unwrap();
        assert_eq!(ui.transcript.entries().count(), 1);
        assert_eq!(ui.transcript.read_anchor(), anchor);
        assert!(!ui.transcript.has_metrics());
        assert!(retained_history_bytes(&state) <= limit);
    }

    #[test]
    fn combined_metrics_overflow_drops_inactive_layout_before_selected_layout_or_history() {
        let mut state = fixture();
        let anchor = state.selected_ui().unwrap().transcript.read_anchor();
        let current = metrics([1]);
        let source_bytes = state.selected_ui().unwrap().transcript.allocated_bytes();
        let limit = source_bytes + current.allocated_bytes();

        let background = SessionId::new();
        let mut background_ui = SessionUi::new(background, 1);
        background_ui
            .transcript
            .retain_metrics(metrics([2]), HISTORY_CACHE_BYTES);
        state.session_ui.insert(background, background_ui);

        assert!(retain_transcript_with_limit(
            &mut state,
            current.clone(),
            limit
        ));
        let ui = state.selected_ui().unwrap();
        assert!(ui.transcript.has_metrics());
        assert_eq!(ui.transcript.allocated_bytes(), limit);
        assert_eq!(ui.transcript.entries().count(), 1);
        assert_eq!(ui.transcript.read_anchor(), anchor);
        assert!(!state.session_ui[&background].transcript.has_metrics());
        assert!(retained_history_bytes(&state) <= limit);
    }
}

#[cfg(test)]
mod focus_state_tests {
    use super::*;

    fn act(state: &mut UiState, action: Action) {
        assert!(update(state, UiEvent::Action(action)).is_empty());
    }

    fn state_with_session() -> UiState {
        let info = bone_app::SessionInfo {
            id: SessionId::new(),
            workspace: bone_app::WorkspaceId::new(),
            title: "focus target".into(),
            archived: false,
        };
        let mut state = UiState::default();
        state.session_rows.push(test_session_row(info.clone()));
        state.selected = Some(info.id);
        state.session_ui.insert(info.id, SessionUi::new(info.id, 1));
        state
    }

    #[test]
    fn session_rail_returns_to_the_center_region_it_came_from() {
        for center in [Focus::SessionTitle, Focus::Composer] {
            let mut state = state_with_session();
            act(&mut state, Action::Focus(center));
            act(&mut state, Action::FocusLeft);
            assert_eq!(state.focus, Focus::Sessions);
            assert_eq!(state.last_center_focus(), center);

            act(&mut state, Action::FocusLeft);
            assert_eq!(state.last_center_focus(), center);

            act(&mut state, Action::FocusRight);
            assert_eq!(state.focus, center);
        }
    }

    #[test]
    fn vertical_navigation_updates_the_remembered_center_region() {
        let mut state = state_with_session();

        act(&mut state, Action::FocusUp);
        assert_eq!(state.focus, Focus::SessionTitle);
        act(&mut state, Action::FocusLeft);
        act(&mut state, Action::FocusRight);
        assert_eq!(state.focus, Focus::SessionTitle);

        act(&mut state, Action::FocusDown);
        assert_eq!(state.focus, Focus::Composer);
        act(&mut state, Action::FocusLeft);
        act(&mut state, Action::FocusRight);
        assert_eq!(state.focus, Focus::Composer);
    }

    #[test]
    fn dragging_a_selection_focuses_the_composer() {
        let mut state = UiState::default();
        state.orphan_draft = "abc".into();
        state.set_focus(Focus::SessionTitle);

        act(
            &mut state,
            test_editor(EditCommand::Point {
                byte: 1,
                extend: true,
            }),
        );

        assert_eq!(state.focus, Focus::Composer);
        assert_eq!(state.editor().selection(), Some(1..3));
    }

    #[test]
    fn session_created_does_not_steal_focus_after_the_request_started() {
        for focus in [Focus::Sessions, Focus::RightRail] {
            let mut state = UiState::default();
            act(&mut state, test_insert("first input"));
            let effects = update(&mut state, UiEvent::Action(Action::Submit));
            assert!(matches!(effects.as_slice(), [Effect::CreateSession { .. }]));
            let request_id = state.pending_create.as_ref().unwrap().request_id;
            act(&mut state, Action::Focus(focus));

            let info = bone_app::SessionInfo {
                id: SessionId::new(),
                workspace: bone_app::WorkspaceId::new(),
                title: "new session".into(),
                archived: false,
            };
            update(
                &mut state,
                UiEvent::SessionCreated {
                    request_id,
                    info: info.clone(),
                },
            );

            assert_eq!(state.selected, Some(info.id));
            assert_eq!(state.focus, focus);
        }
    }

    #[test]
    fn slash_new_focuses_composer_before_async_completion() {
        let mut state = UiState::default();
        state.set_focus(Focus::Sessions);

        act(&mut state, Action::StartSlashCommand);
        act(&mut state, test_insert("new"));
        let effects = update(&mut state, UiEvent::Action(Action::Submit));

        assert!(matches!(effects.as_slice(), [Effect::CreateSession { .. }]));
        assert_eq!(state.focus, Focus::Composer);
    }

    fn state_with_reader_history(focus: Focus) -> (UiState, bone_app::SessionSeq) {
        let sequence = bone_app::SessionSeq(1);
        let runtime = bone_app::RuntimeId::new();
        let info = bone_app::SessionInfo {
            id: SessionId::new(),
            workspace: bone_app::WorkspaceId::new(),
            title: "reader focus".into(),
            archived: false,
        };
        let mut ui = SessionUi::new(info.id, 1);
        ui.transcript.open(bone_app::RecentHistoryPage {
            items: vec![bone_app::HistoryEntry {
                sequence,
                occurred_at: 0,
                event: bone_app::SessionEvent::JobFinished {
                    job: bone_app::JobRef { runtime, id: 1 },
                    outcome: bone_app::OutcomeKind::Completed,
                    summary: "done".into(),
                    remaining: vec![],
                },
            }],
            older_cursor: None,
            snapshot_through: sequence,
        });
        let mut state = UiState::default();
        state.session_rows.push(test_session_row(info.clone()));
        state.selected = Some(info.id);
        state.session_ui.insert(info.id, ui);
        state.set_focus(focus);
        (state, sequence)
    }

    #[test]
    fn reader_restores_the_exact_workspace_focus() {
        for focus in [
            Focus::Sessions,
            Focus::SessionTitle,
            Focus::Composer,
            Focus::RightRail,
        ] {
            let (mut state, sequence) = state_with_reader_history(focus);

            act(&mut state, Action::OpenHistory(sequence));
            assert!(matches!(state.panel, Some(Panel::Reader(_))));
            assert_eq!(state.focus, focus);

            act(&mut state, Action::Escape);
            assert!(state.panel.is_none());
            assert_eq!(state.focus, focus);
        }
    }

    #[test]
    fn title_focus_requires_a_selected_session() {
        let mut state = UiState::default();

        act(&mut state, Action::FocusUp);
        assert_eq!(state.focus, Focus::Composer);
        act(&mut state, Action::Focus(Focus::SessionTitle));
        assert_eq!(state.focus, Focus::Composer);
    }

    #[test]
    fn overview_removal_repairs_an_orphaned_title_focus() {
        let mut state = state_with_session();
        act(&mut state, Action::Focus(Focus::SessionTitle));
        assert!(state.title_edit.is_some());
        let overview_request = state.generation();
        state.overview_request = Some(overview_request);

        update(
            &mut state,
            UiEvent::OverviewLoaded {
                generation: overview_request,
                rows: vec![],
            },
        );

        assert_eq!(state.selected, None);
        assert_eq!(state.focus, Focus::Composer);
        assert!(state.title_edit.is_none());
    }

    #[test]
    fn overview_removal_repairs_the_remembered_title_region() {
        for focus in [Focus::Sessions, Focus::RightRail] {
            let mut state = state_with_session();
            state.set_focus(Focus::SessionTitle);
            state.set_focus(focus);
            assert_eq!(state.last_center_focus(), Focus::SessionTitle);
            let overview_request = state.generation();
            state.overview_request = Some(overview_request);

            update(
                &mut state,
                UiEvent::OverviewLoaded {
                    generation: overview_request,
                    rows: vec![],
                },
            );

            assert_eq!(state.focus, focus);
            assert_eq!(state.last_center_focus(), Focus::Composer);
            act(
                &mut state,
                if focus == Focus::Sessions {
                    Action::FocusRight
                } else {
                    Action::FocusLeft
                },
            );
            assert_eq!(state.focus, Focus::Composer);
        }
    }

    #[test]
    fn resize_repairs_the_real_focus_while_a_panel_is_open() {
        let mut state = state_with_session();
        state.set_focus(Focus::RightRail);
        panel::open_help(&mut state, &mut Vec::new());

        update(
            &mut state,
            UiEvent::Resized {
                width: 100,
                height: 24,
            },
        );
        assert_eq!(state.focus, Focus::Composer);
        assert!(matches!(state.panel, Some(Panel::Help)));
        act(&mut state, Action::Escape);
        assert_eq!(state.focus, Focus::Composer);
    }

    #[test]
    fn attached_slash_command_keeps_composer_focus() {
        let mut state = UiState::default();
        act(&mut state, test_insert("/help"));

        act(&mut state, Action::Submit);

        assert!(matches!(state.panel, Some(Panel::Help)));
        assert_eq!(state.focus, Focus::Composer);
        act(&mut state, Action::Escape);
        assert_eq!(state.focus, Focus::Composer);
    }
}

#[cfg(test)]
mod title_rename_tests {
    use std::sync::Arc;

    use super::*;

    fn session_info(
        workspace: bone_app::WorkspaceId,
        id: SessionId,
        title: impl Into<String>,
    ) -> bone_app::SessionInfo {
        bone_app::SessionInfo {
            id,
            workspace,
            title: title.into(),
            archived: false,
        }
    }

    fn summary(info: bone_app::SessionInfo) -> bone_app::SessionSummary {
        bone_app::SessionSummary {
            session: info,
            created_at: 1,
            message_count: 0,
            latest_reply_preview: None,
            projection_pending: false,
            has_draft: false,
            draft_bytes: 0,
            persisted_runtime: None,
            history_through: bone_app::SessionSeq(0),
        }
    }

    fn row(info: bone_app::SessionInfo) -> SessionNavRow {
        SessionNavRow {
            summary: summary(info),
            needs_attention: false,
        }
    }

    fn fixture() -> (UiState, bone_app::SessionInfo, bone_app::SessionInfo) {
        let workspace = bone_app::WorkspaceId::new();
        let first = session_info(workspace, SessionId::new(), "Alpha");
        let second = session_info(workspace, SessionId::new(), "Beta");
        let mut state = UiState::default();
        let generation = state.generation();
        state.session_rows = vec![row(first.clone()), row(second.clone())];
        state
            .session_ui
            .insert(first.id, SessionUi::new(first.id, generation));
        state
            .session_ui
            .insert(second.id, SessionUi::new(second.id, generation));
        state.selected = Some(first.id);
        state.session_candidate = Some(first.id);
        (state, first, second)
    }

    fn dirty_title(state: &mut UiState, title: &str) {
        update(state, UiEvent::Action(Action::Focus(Focus::SessionTitle)));
        let edit = state.title_edit.as_mut().expect("selected title editor");
        edit.editor
            .apply(EditCommand::Replace { text: title.into() });
    }

    fn rename_effect(effects: &[Effect]) -> (SessionId, u64, String) {
        effects
            .iter()
            .find_map(|effect| match effect {
                Effect::RenameSession {
                    session,
                    request,
                    title,
                } => Some((*session, *request, title.clone())),
                _ => None,
            })
            .expect("rename effect")
    }

    fn title_in_rail(state: &UiState, session: SessionId) -> &str {
        state.session_title(session).expect("listed Session")
    }

    fn committed_title_in_row(state: &UiState, session: SessionId) -> &str {
        &state
            .session_row(session)
            .expect("listed Session")
            .info()
            .title
    }

    fn overview(
        state: &mut UiState,
        first: &bone_app::SessionInfo,
        second: &bone_app::SessionInfo,
    ) -> Vec<Effect> {
        overview_rows(state, vec![row(first.clone()), row(second.clone())])
    }

    fn overview_rows(state: &mut UiState, rows: Vec<SessionNavRow>) -> Vec<Effect> {
        let generation = state.generation();
        state.overview_request = Some(generation);
        update(state, UiEvent::OverviewLoaded { generation, rows })
    }

    fn snapshot(info: bone_app::SessionInfo) -> Arc<bone_app::SessionView> {
        Arc::new(bone_app::SessionView {
            session: info,
            runtime: bone_app::RuntimeState::Detached,
            draft: String::new(),
            inputs: vec![],
            jobs: vec![],
            activity: vec![],
            history_through: bone_app::SessionSeq(0),
            problem: None,
        })
    }

    #[test]
    fn scrolling_and_resizing_do_not_commit_a_dirty_title() {
        let (mut state, _, _) = fixture();
        dirty_title(&mut state, "Local edit");

        for action in [
            Action::ScrollUp(3),
            Action::ScrollSessions { start: 1 },
            Action::BeginPaneResize(crate::layout::PaneDivider::Left),
        ] {
            let effects = update(&mut state, UiEvent::Action(action));
            assert!(
                effects
                    .iter()
                    .all(|effect| !matches!(effect, Effect::RenameSession { .. }))
            );
            assert!(state.title_renames.is_empty());
            assert_eq!(state.title_text(), Some("Local edit"));
        }
    }

    #[test]
    fn title_home_and_end_extend_selection_without_splitting_graphemes() {
        let (mut state, first, _) = fixture();
        let title = "中e\u{301}🙂";
        set_committed_session_title(&mut state, first.id, title.into());
        update(
            &mut state,
            UiEvent::Action(Action::Focus(Focus::SessionTitle)),
        );

        update(
            &mut state,
            UiEvent::Action(test_title_editor(EditCommand::Move {
                cursor: CursorMove::LineStart,
                select: true,
            })),
        );
        let edit = state.title_edit.as_ref().unwrap();
        assert_eq!(edit.editor.cursor(), 0);
        assert_eq!(edit.editor.selection(), Some(0..title.len()));

        update(
            &mut state,
            UiEvent::Action(test_title_editor(EditCommand::Move {
                cursor: CursorMove::LineStart,
                select: false,
            })),
        );
        update(
            &mut state,
            UiEvent::Action(test_title_editor(EditCommand::Move {
                cursor: CursorMove::LineEnd,
                select: true,
            })),
        );
        let edit = state.title_edit.as_ref().unwrap();
        assert_eq!(edit.editor.cursor(), title.len());
        assert_eq!(edit.editor.selection(), Some(0..title.len()));
    }

    #[test]
    fn every_user_move_out_of_the_title_commits_before_focus_changes() {
        for action in [
            Action::FocusLeft,
            Action::FocusDown,
            Action::Focus(Focus::Composer),
            test_editor(EditCommand::Point {
                byte: 0,
                extend: false,
            }),
            test_editor(EditCommand::Point {
                byte: 0,
                extend: true,
            }),
            Action::ClickSubmit,
        ] {
            let (mut state, first, _) = fixture();
            dirty_title(&mut state, "Committed on leave");
            let effects = update(&mut state, UiEvent::Action(action));
            let (session, request, title) = rename_effect(&effects);
            assert_eq!((session, title), (first.id, "Committed on leave".into()));
            assert_ne!(request, 0);
            assert_ne!(state.focus, Focus::SessionTitle);
        }

        let (mut state, first, second) = fixture();
        dirty_title(&mut state, "Committed on switch");
        let effects = update(
            &mut state,
            UiEvent::Action(Action::SelectSession(second.id)),
        );
        let (session, request, title) = rename_effect(&effects);
        assert_eq!((session, title), (first.id, "Committed on switch".into()));
        assert_ne!(request, 0);
        assert_eq!(state.selected, Some(second.id));

        let (mut state, first, _) = fixture();
        dirty_title(&mut state, "Committed on quit");
        let effects = update(&mut state, UiEvent::Action(Action::Quit));
        let (session, request, title) = rename_effect(&effects);
        assert_eq!((session, title), (first.id, "Committed on quit".into()));
        assert_ne!(request, 0);
        let rename = effects
            .iter()
            .position(|effect| matches!(effect, Effect::RenameSession { .. }))
            .unwrap();
        let shutdown = effects
            .iter()
            .position(|effect| matches!(effect, Effect::Shutdown))
            .unwrap();
        assert!(
            rename < shutdown,
            "title is handed to Runtime before shutdown"
        );
    }

    #[test]
    fn switch_keeps_inflight_and_queued_titles_until_the_final_write_finishes() {
        let (mut state, first, second) = fixture();
        dirty_title(&mut state, "First write");
        let (_, first_request, _) =
            rename_effect(&update(&mut state, UiEvent::Action(Action::CommitTitle)));
        dirty_title(&mut state, "Queued write");
        assert!(update(&mut state, UiEvent::Action(Action::CommitTitle)).is_empty());

        let switched = update(
            &mut state,
            UiEvent::Action(Action::SelectSession(second.id)),
        );
        assert!(switched.iter().all(|effect| {
            !matches!(effect, Effect::ReleaseSession { session, .. } if *session == first.id)
        }));

        let retry = update(
            &mut state,
            UiEvent::SessionRenameFailed {
                session: first.id,
                request: first_request,
                message: "old write failed".into(),
            },
        );
        let (_, second_request, title) = rename_effect(&retry);
        assert_eq!(title, "Queued write");
        assert!(
            state.status.is_none(),
            "background failure stays with its Session"
        );

        let finished = update(
            &mut state,
            UiEvent::SessionRenamed {
                session: first.id,
                request: second_request,
                title,
            },
        );
        assert_eq!(title_in_rail(&state, first.id), "Queued write");
        assert!(finished.iter().any(|effect| {
            matches!(effect, Effect::ReleaseSession { session, .. } if *session == first.id)
        }));
    }

    #[test]
    fn exit_flushes_the_latest_queued_title_and_ignores_reversed_callbacks() {
        let (mut state, first, _) = fixture();
        dirty_title(&mut state, "First write");
        let (_, first_request, first_title) =
            rename_effect(&update(&mut state, UiEvent::Action(Action::CommitTitle)));
        dirty_title(&mut state, "Final write");
        assert!(update(&mut state, UiEvent::Action(Action::CommitTitle)).is_empty());

        let exit = update(&mut state, UiEvent::Action(Action::Quit));
        let (_, final_request, final_title) = rename_effect(&exit);
        assert!(final_request > first_request);
        assert!(matches!(exit.last(), Some(Effect::Shutdown)));

        update(
            &mut state,
            UiEvent::SessionRenamed {
                session: first.id,
                request: final_request,
                title: final_title,
            },
        );
        update(
            &mut state,
            UiEvent::SessionRenamed {
                session: first.id,
                request: first_request,
                title: first_title,
            },
        );
        assert_eq!(title_in_rail(&state, first.id), "Final write");
        assert_eq!(committed_title_in_row(&state, first.id), "Final write");
        assert_eq!(state.title_text(), Some("Final write"));
    }

    #[test]
    fn session_created_commits_the_old_title_before_replacing_its_editor() {
        let (mut state, first, _) = fixture();
        let ui = state.session_ui.get_mut(&first.id).unwrap();
        ui.draft = "/new".into();
        let create = update(&mut state, UiEvent::Action(Action::Submit));
        assert!(
            create
                .iter()
                .any(|effect| matches!(effect, Effect::CreateSession { .. }))
        );
        let request_id = state.pending_create.as_ref().unwrap().request_id;
        dirty_title(&mut state, "Old title while /new waits");

        let created = session_info(first.workspace, SessionId::new(), "New conversation");
        let effects = update(
            &mut state,
            UiEvent::SessionCreated {
                request_id,
                info: created.clone(),
            },
        );
        let (session, request, title) = rename_effect(&effects);
        assert_eq!(
            (session, title),
            (first.id, "Old title while /new waits".into())
        );
        assert_ne!(request, 0);
        assert_eq!(state.selected, Some(created.id));
        assert_eq!(
            state.title_edit.as_ref().map(|edit| edit.target),
            Some(created.id)
        );
        assert!(state.title_rename_pending(first.id));
    }

    #[test]
    fn release_generation_changes_do_not_replace_the_session_scoped_editor() {
        let (mut state, first, _) = fixture();
        update(
            &mut state,
            UiEvent::Action(Action::Focus(Focus::SessionTitle)),
        );
        update(
            &mut state,
            UiEvent::Action(test_title_editor(EditCommand::Move {
                cursor: CursorMove::LineEnd,
                select: false,
            })),
        );
        update(&mut state, UiEvent::Action(test_title_insert(" changed")));
        let edited = state.title_text().unwrap().to_owned();
        let old_generation = state.session_ui[&first.id].generation;

        let reopen = update(
            &mut state,
            UiEvent::SessionReleased {
                generation: old_generation,
                receipt: bone_app::SessionReleaseReceipt {
                    session: first.id,
                    status: bone_app::SessionReleaseStatus::Released,
                },
            },
        );
        assert!(matches!(reopen.as_slice(), [Effect::OpenSession { .. }]));
        assert_ne!(state.session_ui[&first.id].generation, old_generation);
        assert_eq!(state.title_text(), Some(edited.as_str()));

        update(
            &mut state,
            UiEvent::Action(test_title_editor(EditCommand::Undo)),
        );
        assert_eq!(state.title_text(), Some("Alpha"));
        update(
            &mut state,
            UiEvent::Action(test_title_editor(EditCommand::Redo)),
        );
        assert_eq!(state.title_text(), Some(edited.as_str()));
        let effects = update(&mut state, UiEvent::Action(Action::CommitTitle));
        assert_eq!(rename_effect(&effects).0, first.id);
    }

    #[test]
    fn manual_intent_wins_before_or_after_an_auto_title_receipt() {
        let (mut state, first, _) = fixture();
        state.session_ui.get_mut(&first.id).unwrap().draft = "first input".into();
        let submitted = update(&mut state, UiEvent::Action(Action::Submit));
        let request_id = submitted
            .iter()
            .find_map(|effect| match effect {
                Effect::Submit { input, .. } => Some(input.request_id),
                _ => None,
            })
            .unwrap();
        dirty_title(&mut state, "Manual title");
        let (_, manual_request, manual_title) =
            rename_effect(&update(&mut state, UiEvent::Action(Action::CommitTitle)));
        let receipt_effects = update(
            &mut state,
            UiEvent::Submitted {
                session: first.id,
                request_id,
            },
        );
        assert!(
            receipt_effects
                .iter()
                .all(|effect| !matches!(effect, Effect::AutoTitle { .. }))
        );
        update(
            &mut state,
            UiEvent::SessionRenamed {
                session: first.id,
                request: manual_request,
                title: manual_title,
            },
        );
        update(
            &mut state,
            UiEvent::SessionAutoTitled {
                session: first.id,
                request: manual_request.wrapping_add(10),
                title: "Late automatic title".into(),
            },
        );
        assert_eq!(title_in_rail(&state, first.id), "Manual title");

        let (mut state, first, _) = fixture();
        state.session_ui.get_mut(&first.id).unwrap().submitting = Some(PendingSubmission {
            request_id: RequestId::new(),
            text: "first input".into(),
            draft_revision: 0,
            failed: false,
            reply_to: None,
        });
        let submission = state.session_ui[&first.id]
            .submitting
            .as_ref()
            .unwrap()
            .request_id;
        let auto = update(
            &mut state,
            UiEvent::Submitted {
                session: first.id,
                request_id: submission,
            },
        );
        let auto_request = auto
            .iter()
            .find_map(|effect| match effect {
                Effect::AutoTitle { request, .. } => Some(*request),
                _ => None,
            })
            .expect("auto title was already scheduled");
        dirty_title(&mut state, "Manual after submit");
        let (_, manual_request, manual_title) =
            rename_effect(&update(&mut state, UiEvent::Action(Action::CommitTitle)));
        update(
            &mut state,
            UiEvent::SessionRenamed {
                session: first.id,
                request: manual_request,
                title: manual_title,
            },
        );
        update(
            &mut state,
            UiEvent::SessionAutoTitled {
                session: first.id,
                request: auto_request,
                title: "Older automatic title".into(),
            },
        );
        assert_eq!(title_in_rail(&state, first.id), "Manual after submit");
    }

    #[test]
    fn auto_title_receipt_survives_a_session_generation_change() {
        let (mut state, first, _) = fixture();
        update(
            &mut state,
            UiEvent::SessionReleased {
                generation: 1,
                receipt: bone_app::SessionReleaseReceipt {
                    session: first.id,
                    status: bone_app::SessionReleaseStatus::Released,
                },
            },
        );
        assert_ne!(state.session_ui[&first.id].generation, 1);

        update(
            &mut state,
            UiEvent::SessionAutoTitled {
                session: first.id,
                request: 7,
                title: "Durable automatic title".into(),
            },
        );
        assert_eq!(title_in_rail(&state, first.id), "Durable automatic title");
        assert_eq!(
            committed_title_in_row(&state, first.id),
            "Durable automatic title"
        );
    }

    #[test]
    fn authoritative_titles_update_the_baseline_without_publishing_dirty_editor_text() {
        for authoritative in ["Alpha", "External title"] {
            let (mut state, first, mut second) = fixture();
            dirty_title(&mut state, "Uncommitted editor text");
            let mut changed = first.clone();
            changed.title = authoritative.into();
            second.title = "Beta".into();
            overview(&mut state, &changed, &second);

            assert_eq!(title_in_rail(&state, first.id), authoritative);
            assert_eq!(committed_title_in_row(&state, first.id), authoritative);
            let edit = state.title_edit.as_ref().unwrap();
            assert_eq!(edit.editor.text(), "Uncommitted editor text");
            assert_eq!(edit.original, authoritative);

            update(&mut state, UiEvent::Action(Action::CancelTitle));
            assert_eq!(title_in_rail(&state, first.id), authoritative);
            assert_eq!(state.title_text(), Some(authoritative));
        }
    }

    #[test]
    fn pending_and_confirmed_titles_protect_against_stale_metadata_then_reconcile() {
        let (mut state, first, second) = fixture();
        dirty_title(&mut state, "Durable local title");
        let (_, request, title) =
            rename_effect(&update(&mut state, UiEvent::Action(Action::CommitTitle)));
        overview(&mut state, &first, &second);
        assert_eq!(title_in_rail(&state, first.id), "Durable local title");
        assert_eq!(committed_title_in_row(&state, first.id), "Alpha");

        update(
            &mut state,
            UiEvent::SessionRenamed {
                session: first.id,
                request,
                title,
            },
        );
        overview(&mut state, &first, &second);
        assert_eq!(title_in_rail(&state, first.id), "Durable local title");

        let mut authoritative = first.clone();
        authoritative.title = "Durable local title".into();
        overview(&mut state, &authoritative, &second);
        assert_eq!(
            committed_title_in_row(&state, first.id),
            "Durable local title"
        );
        assert!(
            state
                .title_renames
                .get(&first.id)
                .is_none_or(|queue| queue.confirmed.is_none())
        );
    }

    #[test]
    fn session_snapshot_title_merges_into_ui_and_respects_a_pending_override() {
        let (mut state, first, _) = fixture();
        let mut external = first.clone();
        external.title = "Snapshot title".into();
        update(
            &mut state,
            UiEvent::SessionChanged {
                session: first.id,
                generation: 1,
                snapshot: snapshot(external),
            },
        );
        assert_eq!(title_in_rail(&state, first.id), "Snapshot title");
        assert_eq!(committed_title_in_row(&state, first.id), "Snapshot title");

        dirty_title(&mut state, "Pending local title");
        let effects = update(&mut state, UiEvent::Action(Action::CommitTitle));
        assert!(matches!(effects.as_slice(), [Effect::RenameSession { .. }]));
        let mut stale = first.clone();
        stale.title = "Stale snapshot".into();
        update(
            &mut state,
            UiEvent::SessionChanged {
                session: first.id,
                generation: 1,
                snapshot: snapshot(stale),
            },
        );
        assert_eq!(title_in_rail(&state, first.id), "Pending local title");
        assert_eq!(state.title_text(), Some("Pending local title"));
    }

    #[test]
    fn final_failure_restores_header_and_rail_to_the_committed_baseline() {
        let (mut state, first, second) = fixture();
        dirty_title(&mut state, "Failed title");
        let (_, request, _) =
            rename_effect(&update(&mut state, UiEvent::Action(Action::CommitTitle)));
        overview(&mut state, &first, &second);
        assert_eq!(title_in_rail(&state, first.id), "Failed title");

        update(
            &mut state,
            UiEvent::SessionRenameFailed {
                session: first.id,
                request,
                message: "Could not save title".into(),
            },
        );
        assert_eq!(title_in_rail(&state, first.id), "Alpha");
        assert_eq!(committed_title_in_row(&state, first.id), "Alpha");
        assert_eq!(state.title_text(), Some("Alpha"));
        assert_eq!(state.status.as_deref(), Some("Could not save title"));

        update(
            &mut state,
            UiEvent::Action(test_title_editor(EditCommand::Undo)),
        );
        assert_eq!(state.title_text(), Some("Alpha"));
    }

    #[test]
    fn cancelling_an_edit_keeps_a_late_auto_title_out_of_the_editor() {
        let (mut state, first, _) = fixture();
        dirty_title(&mut state, "Cancelled local edit");
        let edited_revision = state.title_edit.as_ref().unwrap().editor.revision();

        update(&mut state, UiEvent::Action(Action::CancelTitle));
        let cancelled_revision = state.title_edit.as_ref().unwrap().editor.revision();
        assert!(cancelled_revision > edited_revision);
        assert_eq!(state.title_text(), Some("Alpha"));

        update(
            &mut state,
            UiEvent::SessionAutoTitled {
                session: first.id,
                request: 99,
                title: "Late automatic title".into(),
            },
        );
        assert_eq!(state.title_text(), Some("Alpha"));
    }

    #[test]
    fn confirmed_title_survives_stale_summary_and_becomes_the_next_failure_baseline() {
        let (mut state, first, second) = fixture();
        dirty_title(&mut state, "Confirmed title");
        let (_, request, title) =
            rename_effect(&update(&mut state, UiEvent::Action(Action::CommitTitle)));
        update(
            &mut state,
            UiEvent::SessionRenamed {
                session: first.id,
                request,
                title,
            },
        );

        let mut stale = row(first.clone());
        stale.summary.message_count = 41;
        stale.summary.latest_reply_preview = Some("New reply on stale metadata".into());
        overview_rows(&mut state, vec![stale, row(second.clone())]);
        let refreshed = state.session_row(first.id).unwrap();
        assert_eq!(refreshed.info().title, "Confirmed title");
        assert_eq!(refreshed.summary.message_count, 41);
        assert_eq!(
            refreshed.summary.latest_reply_preview.as_deref(),
            Some("New reply on stale metadata")
        );

        dirty_title(&mut state, "Next title");
        let (_, request, _) =
            rename_effect(&update(&mut state, UiEvent::Action(Action::CommitTitle)));
        update(
            &mut state,
            UiEvent::SessionRenameFailed {
                session: first.id,
                request,
                message: "Could not save next title".into(),
            },
        );
        assert_eq!(committed_title_in_row(&state, first.id), "Confirmed title");
        assert_eq!(title_in_rail(&state, first.id), "Confirmed title");
        assert_eq!(state.title_text(), Some("Confirmed title"));
    }

    #[test]
    fn confirmed_title_survives_a_temporarily_missing_navigation_row() {
        let (mut state, first, second) = fixture();
        dirty_title(&mut state, "Confirmed title");
        let (_, request, title) =
            rename_effect(&update(&mut state, UiEvent::Action(Action::CommitTitle)));
        update(
            &mut state,
            UiEvent::SessionRenamed {
                session: first.id,
                request,
                title,
            },
        );

        overview_rows(&mut state, vec![row(second.clone())]);
        assert!(state.session_row(first.id).is_none());
        overview_rows(&mut state, vec![row(first.clone()), row(second.clone())]);
        assert_eq!(committed_title_in_row(&state, first.id), "Confirmed title");
        assert_eq!(title_in_rail(&state, first.id), "Confirmed title");

        select_session(&mut state, first.id, &mut Vec::new());
        dirty_title(&mut state, "Next title");
        let (_, request, _) =
            rename_effect(&update(&mut state, UiEvent::Action(Action::CommitTitle)));
        update(
            &mut state,
            UiEvent::SessionRenameFailed {
                session: first.id,
                request,
                message: "Could not save next title".into(),
            },
        );
        assert_eq!(committed_title_in_row(&state, first.id), "Confirmed title");
        assert_eq!(title_in_rail(&state, first.id), "Confirmed title");
        assert_eq!(state.title_text(), Some("Confirmed title"));
    }

    #[test]
    fn stale_session_generation_cannot_mutate_navigation_or_title_state() {
        let (mut state, first, _) = fixture();
        dirty_title(&mut state, "Pending title");
        let (_, request, _) =
            rename_effect(&update(&mut state, UiEvent::Action(Action::CommitTitle)));
        state.session_ui.get_mut(&first.id).unwrap().generation = 2;
        let before = state.title_edit.as_ref().unwrap();
        let original = before.original.clone();
        let text = before.editor.text().to_owned();
        let revision = before.editor.revision();

        let mut stale = first.clone();
        stale.title = "Stale snapshot title".into();
        stale.archived = true;
        update(
            &mut state,
            UiEvent::SessionChanged {
                session: first.id,
                generation: 1,
                snapshot: snapshot(stale),
            },
        );

        let row = state.session_row(first.id).unwrap();
        assert_eq!(row.info().title, "Alpha");
        assert!(!row.info().archived);
        let edit = state.title_edit.as_ref().unwrap();
        assert_eq!(edit.original, original);
        assert_eq!(edit.editor.text(), text);
        assert_eq!(edit.editor.revision(), revision);
        assert!(
            state.title_renames[&first.id]
                .pending
                .contains_key(&request)
        );
        assert!(state.session_ui[&first.id].snapshot.is_none());
    }
}
