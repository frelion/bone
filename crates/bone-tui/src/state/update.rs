use bone_app::{RequestId, SessionId, SubmitInput};
use unicode_segmentation::UnicodeSegmentation;

use super::{model::*, protocol::*};

pub fn update(state: &mut UiState, event: UiEvent) -> Vec<Effect> {
    if matches!(event, UiEvent::Tick | UiEvent::Action(Action::Noop)) {
        return Vec::new();
    }
    state.dirty = true;
    let mut effects = Vec::new();
    match event {
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
            }
        }
        UiEvent::SessionChanged {
            session,
            generation,
            snapshot,
        } => {
            let selected = state.selected == Some(session);
            if let Some(ui) = current_generation_mut(state, session, generation) {
                let through = snapshot.history_through;
                ui.info = snapshot.session.clone();
                ui.snapshot = Some(snapshot);
                if through > ui.history_cursor && !ui.history_loading && !ui.newer_history_missing {
                    if ui.scroll_from_tail > 0 || ui.older_loading {
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
                if ui.scroll_from_tail > 0 || ui.older_loading {
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
                ui.history.clear();
                ui.history_bytes = 0;
                for entry in page.items {
                    ui.history_bytes = ui.history_bytes.saturating_add(history_entry_bytes(&entry));
                    ui.history.push_back(entry);
                }
                ui.history_cursor = page.snapshot_through;
                ui.older_cursor = page.older_cursor;
                ui.scroll_from_tail = 0;
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
            generation,
            request_id,
            ..
        } => {
            if let Some(ui) = current_generation_mut(state, session, generation)
                && ui
                    .submitting
                    .as_ref()
                    .is_some_and(|pending| pending.request_id == request_id)
                && let Some(pending) = ui.submitting.take()
            {
                if ui.draft_revision == pending.draft_revision && ui.draft == pending.text {
                    ui.draft.clear();
                    ui.draft_cursor = 0;
                    ui.draft_revision = ui.draft_revision.wrapping_add(1);
                    effects.push(Effect::SaveDraft {
                        session,
                        generation,
                        revision: ui.draft_revision,
                        text: String::new(),
                    });
                }
                effects.push(Effect::AutoTitle {
                    session,
                    generation,
                    first_input: pending.text,
                });
            }
        }
        UiEvent::SubmitFailed {
            session,
            generation,
            request_id,
            message,
        } => {
            if let Some(ui) = current_generation_mut(state, session, generation)
                && let Some(pending) = &mut ui.submitting
                && pending.request_id == request_id
            {
                pending.failed = true;
                state.status = Some(message);
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
                clear_create_source(state, pending.source, &mut effects);
                if !state.sessions.iter().any(|session| session.id == info.id) {
                    state.sessions.insert(0, info.clone());
                }
                select_session(state, info.id, &mut effects);
                state.focus = Focus::Composer;
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
                state.status = Some(message);
            }
        }
        UiEvent::Resized => {}
        UiEvent::Tick => unreachable!(),
    }
    trim_global_history(state);
    effects
}

fn handle_action(state: &mut UiState, action: Action, effects: &mut Vec<Effect>) {
    match action {
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
        Action::SelectPrevious => move_session(state, -1, effects),
        Action::SelectNext => move_session(state, 1, effects),
        Action::SelectSession(session) => {
            select_session(state, session, effects);
            state.focus = Focus::Sessions;
        }
        Action::SelectSlashPrevious => move_slash(state, -1),
        Action::SelectSlashNext => move_slash(state, 1),
        Action::CompleteSlash => complete_slash(state),
        Action::ExecuteSlash(index) => {
            state.slash_selection = index;
            submit(state, effects);
        }
        Action::Input(value) => insert_text(state, &value.to_string()),
        Action::Paste(text) => insert_text(state, &text),
        Action::Backspace => delete_before_cursor(state),
        Action::Delete => delete_after_cursor(state),
        Action::CursorLeft => move_cursor(state, -1),
        Action::CursorRight => move_cursor(state, 1),
        Action::CursorHome => set_cursor_line_edge(state, false),
        Action::CursorEnd => set_cursor_line_edge(state, true),
        Action::InsertNewline => insert_text(state, "\n"),
        Action::Submit => submit(state, effects),
        Action::Escape => {
            if !state.slash_matches().is_empty() {
                state.slash_dismissed = Some(state.draft_identity());
                state.status = None;
            } else {
                effects.extend(stop_selected(state));
            }
        }
        Action::ScrollUp { amount, metrics } => scroll_up(state, amount, metrics, effects),
        Action::ScrollDown(amount) => {
            if let Some(ui) = state.selected_ui_mut() {
                if ui.newer_history_missing && !ui.recent_loading {
                    ui.recent_loading = true;
                    effects.push(Effect::ReloadRecentHistory {
                        session: ui.info.id,
                        generation: ui.generation,
                    });
                    return;
                }
                ui.scroll_from_tail = ui.scroll_from_tail.saturating_sub(amount);
                if ui.scroll_from_tail == 0 {
                    ui.unread = 0;
                }
            }
        }
        Action::Stop => effects.extend(stop_selected(state)),
        Action::Quit | Action::Terminate => {
            state.quitting = true;
            effects.push(Effect::Shutdown);
        }
    }
}

fn submit(state: &mut UiState, effects: &mut Vec<Effect>) {
    if state.focus != Focus::Composer {
        return;
    }
    let text = state.draft().to_owned();
    if text.trim().is_empty() {
        return;
    }
    if let Some(command) = text.trim_start().strip_prefix('/') {
        execute_command(state, command, effects);
        return;
    }
    let Some(ui) = state.selected_ui_mut() else {
        state.status = Some("Create a session with /new before sending this request".into());
        return;
    };
    if ui
        .submitting
        .as_ref()
        .is_some_and(|pending| !pending.failed)
    {
        return;
    }
    let request_id = ui
        .submitting
        .as_ref()
        .filter(|pending| pending.failed && pending.text == text)
        .map_or_else(RequestId::new, |pending| pending.request_id);
    let revision = ui.draft_revision;
    ui.submitting = Some(PendingSubmission {
        request_id,
        text: text.clone(),
        draft_revision: revision,
        failed: false,
    });
    let mut input = SubmitInput::new(text);
    input.request_id = request_id;
    effects.push(Effect::Submit {
        session: ui.info.id,
        generation: ui.generation,
        input,
    });
}

fn execute_command(state: &mut UiState, raw: &str, effects: &mut Vec<Effect>) {
    let trimmed = raw.trim();
    let mut parts = trimmed.splitn(2, char::is_whitespace);
    let typed = parts.next().unwrap_or_default();
    let argument = parts.next().unwrap_or_default().trim();
    let selected = if argument.is_empty() {
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
                    revision: state.orphan_revision,
                    text: state.orphan_draft.clone(),
                },
                |ui| DraftSource::Session {
                    id: ui.info.id,
                    generation: ui.generation,
                    revision: ui.draft_revision,
                    text: ui.draft.clone(),
                },
            );
            let title: String = if argument.is_empty() {
                "新会话".into()
            } else {
                argument.into()
            };
            let provisional = argument.is_empty();
            state.pending_create = Some(PendingCreate {
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
            state.status = Some(
                COMMANDS
                    .iter()
                    .map(|c| format!("/{} {}", c.name, c.usage))
                    .collect::<Vec<_>>()
                    .join("  "),
            );
            clear_current_draft(state, effects);
        }
        CommandKind::Quit if argument.is_empty() => {
            clear_current_draft(state, effects);
            state.quitting = true;
            effects.push(Effect::Shutdown);
        }
        _ => state.status = Some(format!("Usage: /{} {}", selected.name, selected.usage)),
    }
}

fn create_source_matches(state: &UiState, source: &DraftSource) -> bool {
    match source {
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
    if let Some(ui) = state.selected_ui_mut() {
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
        state.orphan_draft.clear();
        state.orphan_cursor = 0;
        state.orphan_revision = state.orphan_revision.wrapping_add(1);
    }
    state.slash_selection = 0;
    state.slash_dismissed = None;
}

fn clear_create_source(state: &mut UiState, source: DraftSource, effects: &mut Vec<Effect>) {
    match source {
        DraftSource::Orphan { revision, text }
            if state.orphan_revision == revision && state.orphan_draft == text =>
        {
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

fn insert_text(state: &mut UiState, text: &str) {
    state.status = None;
    state.slash_selection = 0;
    state.slash_dismissed = None;
    if let Some(ui) = state.selected_ui_mut() {
        ui.draft.insert_str(ui.draft_cursor, text);
        ui.draft_cursor += text.len();
        ui.draft_revision = ui.draft_revision.wrapping_add(1);
    } else {
        state.orphan_draft.insert_str(state.orphan_cursor, text);
        state.orphan_cursor += text.len();
        state.orphan_revision = state.orphan_revision.wrapping_add(1);
    }
}

fn delete_before_cursor(state: &mut UiState) {
    let changed = if let Some(ui) = state.selected_ui_mut() {
        let previous = ui.draft[..ui.draft_cursor]
            .grapheme_indices(true)
            .next_back()
            .map(|(index, _)| index);
        if let Some(index) = previous {
            ui.draft.drain(index..ui.draft_cursor);
            ui.draft_cursor = index;
            ui.draft_revision = ui.draft_revision.wrapping_add(1);
            true
        } else {
            false
        }
    } else {
        let previous = state.orphan_draft[..state.orphan_cursor]
            .grapheme_indices(true)
            .next_back()
            .map(|(index, _)| index);
        if let Some(index) = previous {
            state.orphan_draft.drain(index..state.orphan_cursor);
            state.orphan_cursor = index;
            state.orphan_revision = state.orphan_revision.wrapping_add(1);
            true
        } else {
            false
        }
    };
    if changed {
        state.status = None;
        state.slash_selection = 0;
        state.slash_dismissed = None;
    }
}

fn delete_after_cursor(state: &mut UiState) {
    let changed = if let Some(ui) = state.selected_ui_mut() {
        let len = ui.draft[ui.draft_cursor..]
            .graphemes(true)
            .next()
            .map(str::len);
        if let Some(len) = len {
            ui.draft.drain(ui.draft_cursor..ui.draft_cursor + len);
            ui.draft_revision = ui.draft_revision.wrapping_add(1);
            true
        } else {
            false
        }
    } else {
        let len = state.orphan_draft[state.orphan_cursor..]
            .graphemes(true)
            .next()
            .map(str::len);
        if let Some(len) = len {
            state
                .orphan_draft
                .drain(state.orphan_cursor..state.orphan_cursor + len);
            state.orphan_revision = state.orphan_revision.wrapping_add(1);
            true
        } else {
            false
        }
    };
    if changed {
        state.status = None;
        state.slash_selection = 0;
        state.slash_dismissed = None;
    }
}

fn move_cursor(state: &mut UiState, delta: isize) {
    if let Some(ui) = state.selected_ui_mut() {
        ui.draft_cursor = moved_cursor(&ui.draft, ui.draft_cursor, delta);
    } else {
        state.orphan_cursor = moved_cursor(&state.orphan_draft, state.orphan_cursor, delta);
    }
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
    if let Some(ui) = state.selected_ui_mut() {
        ui.draft_cursor = line_edge(&ui.draft, ui.draft_cursor, end);
    } else {
        state.orphan_cursor = line_edge(&state.orphan_draft, state.orphan_cursor, end);
    }
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
            ui.draft = replacement;
            ui.draft_cursor = ui.draft.len();
            ui.draft_revision = ui.draft_revision.wrapping_add(1);
        } else {
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

fn move_session(state: &mut UiState, delta: isize, effects: &mut Vec<Effect>) {
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
        .selected
        .and_then(|id| available.iter().position(|candidate| *candidate == id))
        .unwrap_or(0);
    let next = (current as isize + delta).clamp(0, available.len() as isize - 1) as usize;
    select_session(state, available[next], effects);
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
        ui.scroll_from_tail = metrics.as_ref().map_or_else(
            || ui.scroll_from_tail.saturating_add(amount),
            |metrics| {
                ui.scroll_from_tail
                    .saturating_add(amount)
                    .min(metrics.max_scroll())
            },
        );
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
        .filter(|ui| {
            ui.snapshot.as_ref().is_some_and(|snapshot| {
                matches!(snapshot.runtime, bone_app::RuntimeState::Running { .. })
                    || snapshot.jobs.iter().any(|job| {
                        matches!(
                            job.state,
                            bone_app::JobState::Running | bone_app::JobState::Waiting(_)
                        )
                    })
            })
        })
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
        let Some(entry) = ui.history.pop_front() else {
            break;
        };
        ui.history_bytes = ui.history_bytes.saturating_sub(history_entry_bytes(&entry));
    }
}

fn trim_history_back(ui: &mut SessionUi) -> Vec<bone_app::SessionSeq> {
    let mut evicted = Vec::new();
    while ui.history.len() > HISTORY_CACHE_ITEMS || ui.history_bytes > HISTORY_CACHE_BYTES {
        let Some(entry) = ui.history.pop_back() else {
            break;
        };
        ui.history_bytes = ui.history_bytes.saturating_sub(history_entry_bytes(&entry));
        evicted.push(entry.sequence);
        ui.newer_history_missing = true;
    }
    evicted
}

fn trim_global_history(state: &mut UiState) {
    let mut total = state
        .session_ui
        .values()
        .map(|ui| ui.history_bytes)
        .sum::<usize>();
    while total > HISTORY_CACHE_BYTES {
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
        let Some(entry) = ui.history.pop_front() else {
            break;
        };
        let bytes = history_entry_bytes(&entry);
        ui.history_bytes = ui.history_bytes.saturating_sub(bytes);
        total = total.saturating_sub(bytes);
    }
}

#[cfg(test)]
mod tests {
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
        assert!(effects.is_empty());
        assert_eq!(state.orphan_draft, "keep me");
    }
}
