use bone_app::{AttentionItem, RequestId, SessionId, SessionInfo, SubmitInput};
use unicode_segmentation::UnicodeSegmentation;

use super::{model::*, protocol::*};

pub(super) const EVIDENCE_VIEW_BYTES: usize = 1024 * 1024;

pub fn update(state: &mut UiState, event: UiEvent) -> Vec<Effect> {
    if matches!(event, UiEvent::Tick | UiEvent::Action(Action::Noop)) {
        return Vec::new();
    }
    if matches!(event, UiEvent::PersistDraftsRequested) {
        let mut effects = Vec::new();
        for ui in state.session_ui.values_mut() {
            if ui.snapshot.is_some()
                && ui.draft_revision > ui.saved_draft_revision
                && ui.draft_revision > ui.queued_draft_revision
            {
                ui.queued_draft_revision = ui.draft_revision;
                effects.push(Effect::SaveDraft {
                    session: ui.info.id,
                    generation: ui.generation,
                    revision: ui.draft_revision,
                    text: ui.draft.clone(),
                });
            }
        }
        return effects;
    }
    let mut effects = Vec::new();
    match event {
        UiEvent::Action(action) => handle_action(state, action, &mut effects),
        UiEvent::WorkspaceOpened {
            id,
            label,
            sessions,
            attention,
            attention_projection_pending,
            unresolved_writes,
        } => {
            state.workspace = Some((id, label));
            state.workspace_changes = WorkspaceChangesUi::default();
            state.sessions = sessions;
            state.attention = attention;
            state.attention_projection_pending = attention_projection_pending;
            state.unresolved_writes = unresolved_writes;
            state.attention_selection = state
                .attention_selection
                .min(state.attention.len().saturating_sub(1));
            if state.selected.is_none()
                && let Some(info) = state.sessions.iter().find(|item| !item.archived).cloned()
            {
                select_session(state, info.id, &mut effects);
            }
        }
        UiEvent::SessionOpened {
            session,
            generation,
            snapshot,
            history,
        } => {
            let mut merged_drafts = false;
            if let Some(ui) = current_generation_mut(state, session, generation) {
                let history_through = snapshot.history_through;
                let was_hydrated = ui.hydrated_once;
                if !ui.hydrated_once && ui.draft_revision == 0 {
                    ui.draft = snapshot.draft.clone();
                    ui.saved_draft = snapshot.draft.clone();
                } else if !ui.hydrated_once {
                    let local = std::mem::take(&mut ui.draft);
                    ui.saved_draft = snapshot.draft.clone();
                    ui.draft = match (snapshot.draft.is_empty(), local.is_empty()) {
                        (false, false) => {
                            merged_drafts = true;
                            format!("{}\n{}", snapshot.draft, local)
                        }
                        (false, true) => snapshot.draft.clone(),
                        (true, false) => local,
                        (true, true) => String::new(),
                    };
                    ui.draft_revision = ui.draft_revision.wrapping_add(1);
                    ui.queued_draft_revision = ui.draft_revision;
                    effects.push(Effect::SaveDraft {
                        session,
                        generation,
                        revision: ui.draft_revision,
                        text: ui.draft.clone(),
                    });
                } else if ui.draft_revision == ui.saved_draft_revision {
                    ui.draft = snapshot.draft.clone();
                    ui.saved_draft = snapshot.draft.clone();
                } else {
                    // After the first hydration this UI already owns a complete
                    // draft buffer, not a suffix. A release/save acknowledgement
                    // can be reordered, so concatenating the durable snapshot
                    // would duplicate text. Keep the newer local buffer and
                    // persist it again under the new generation below.
                    ui.saved_draft = snapshot.draft.clone();
                }
                if was_hydrated
                    && ui.draft_revision > ui.saved_draft_revision
                    && ui.draft_revision > ui.queued_draft_revision
                {
                    ui.queued_draft_revision = ui.draft_revision;
                    effects.push(Effect::SaveDraft {
                        session,
                        generation,
                        revision: ui.draft_revision,
                        text: ui.draft.clone(),
                    });
                }
                ui.hydrated_once = true;
                if let Some(mut pending) = ui.submitting.take() {
                    if snapshot
                        .inputs
                        .iter()
                        .any(|input| input.request_id == pending.request_id)
                    {
                        if ui.reply_to == pending.reply_to {
                            ui.reply_to = None;
                        }
                        if pending.clear_on_receipt && ui.draft.starts_with(&pending.text) {
                            ui.draft.drain(..pending.text.len());
                            ui.draft_revision = ui.draft_revision.wrapping_add(1);
                            ui.queued_draft_revision = ui.saved_draft_revision;
                            effects.push(Effect::SaveDraft {
                                session,
                                generation,
                                revision: ui.draft_revision,
                                text: ui.draft.clone(),
                            });
                        }
                    } else {
                        pending.failed = true;
                        ui.submitting = Some(pending);
                    }
                }
                ui.snapshot = Some(snapshot);
                ui.history_cursor = ui.history_cursor.max(history.snapshot_through);
                ui.history_has_more = ui.history_cursor < history_through;
                if ui.history.is_empty() {
                    ui.older_cursor = history.older_cursor;
                }
                for item in history.items {
                    if ui
                        .history
                        .back()
                        .is_none_or(|existing| existing.sequence < item.sequence)
                    {
                        ui.history_bytes =
                            ui.history_bytes.saturating_add(history_entry_bytes(&item));
                        ui.history.push_back(item);
                    }
                }
                queue_history_until(ui, history_through, &mut effects);
            }
            if merged_drafts {
                state.status = Some("已把打开前输入追加到原有草稿，未覆盖任何内容".into());
            }
        }
        UiEvent::SessionChanged {
            session,
            generation,
            snapshot,
        } => {
            let is_selected = state.selected == Some(session);
            if let Some(ui) = current_generation_mut(state, session, generation) {
                let history_through = snapshot.history_through;
                ui.snapshot = Some(snapshot);
                queue_history_until(ui, history_through, &mut effects);
                if !is_selected {
                    ui.unread = ui.unread.saturating_add(1);
                }
            }
        }
        UiEvent::SessionReleased {
            generation,
            receipt,
        } if state
            .session_ui
            .get(&receipt.session)
            .is_some_and(|ui| ui.generation == generation) =>
        {
            match receipt.status {
                bone_app::SessionReleaseStatus::Retained(_) => {
                    if let Some(ui) = state.session_ui.get_mut(&receipt.session) {
                        ui.release_requested = state.selected != Some(receipt.session);
                    }
                }
                bone_app::SessionReleaseStatus::Released
                | bone_app::SessionReleaseStatus::NotOpen => {
                    let reopen = state.selected == Some(receipt.session);
                    let generation = state.generation();
                    if let Some(ui) = state.session_ui.get_mut(&receipt.session) {
                        ui.snapshot = None;
                        ui.release_requested = false;
                        ui.queued_draft_revision = ui.saved_draft_revision;
                        ui.history_loading = false;
                        ui.older_loading = false;
                        ui.recent_reloading = false;
                        ui.generation = generation;
                        if reopen {
                            effects.push(Effect::OpenSession {
                                session: receipt.session,
                                generation,
                            });
                        }
                    }
                }
            }
        }
        UiEvent::SessionReleased { .. } => {}
        UiEvent::HistoryLoaded {
            session,
            generation,
            page,
        } => {
            let results_changed = page
                .items
                .iter()
                .any(|entry| matches!(entry.event, bone_app::SessionEvent::JobFinished { .. }));
            if let Some(ui) = current_generation_mut(state, session, generation) {
                ui.history_loading = false;
                if !ui.newer_history_missing {
                    let appended = page.items.len();
                    ui.push_history(page);
                    if ui.scroll_from_tail > 0 {
                        ui.scroll_from_tail = ui.scroll_from_tail.saturating_add(appended);
                    }
                    let history_through = ui
                        .snapshot
                        .as_ref()
                        .map_or(ui.history_cursor, |view| view.history_through);
                    queue_history_until(ui, history_through, &mut effects);
                }
            }
            if results_changed {
                request_results(state, session, &mut effects);
            }
        }
        UiEvent::OlderHistoryLoaded {
            session,
            generation,
            page,
        } => {
            if let Some(ui) = current_generation_mut(state, session, generation) {
                ui.older_loading = false;
                ui.older_cursor = page.older_cursor;
                let previous_scroll = ui.scroll_from_tail;
                let mut inserted = 0usize;
                for item in page.items.into_iter().rev() {
                    if ui
                        .history
                        .front()
                        .is_none_or(|existing| item.sequence < existing.sequence)
                    {
                        ui.history_bytes =
                            ui.history_bytes.saturating_add(history_entry_bytes(&item));
                        ui.history.push_front(item);
                        inserted = inserted.saturating_add(1);
                    }
                }
                let mut evicted = 0usize;
                while ui.history.len() > HISTORY_CACHE_ITEMS {
                    if let Some(item) = ui.history.pop_back() {
                        ui.history_bytes =
                            ui.history_bytes.saturating_sub(history_entry_bytes(&item));
                        ui.newer_history_missing = true;
                        evicted = evicted.saturating_add(1);
                    }
                }
                if inserted > 0 {
                    ui.scroll_from_tail = previous_scroll
                        .saturating_sub(evicted)
                        .saturating_add(1)
                        .min(ui.history.len().saturating_sub(1));
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
                ui.older_cursor = page.older_cursor;
                ui.history_cursor = page.snapshot_through;
                for item in page.items {
                    ui.history_bytes = ui.history_bytes.saturating_add(history_entry_bytes(&item));
                    ui.history.push_back(item);
                }
                ui.scroll_from_tail = 0;
                ui.recent_reloading = false;
                ui.history_window_stale = false;
                ui.newer_history_missing = false;
            }
        }
        UiEvent::RefreshOverviewRequested => {
            if state.overview_pending {
                return effects;
            }
            state.overview_pending = true;
            state.overview_generation = state.overview_generation.wrapping_add(1).max(1);
            effects.push(Effect::RefreshOverview {
                generation: state.overview_generation,
            });
            effects.extend(state.session_ui.values().filter_map(|ui| {
                (ui.release_requested
                    && state.selected != Some(ui.info.id)
                    && ui.draft_revision <= ui.saved_draft_revision)
                    .then_some(Effect::ReleaseSession {
                        session: ui.info.id,
                        generation: ui.generation,
                    })
            }));
            return effects;
        }
        UiEvent::OverviewLoaded {
            generation,
            sessions,
            attention,
            attention_projection_pending,
            unresolved_writes,
        } => {
            if generation != state.overview_generation {
                return effects;
            }
            state.overview_pending = false;
            if state.sessions == sessions
                && state.attention == attention
                && state.attention_projection_pending == attention_projection_pending
                && state.unresolved_writes == unresolved_writes
            {
                return effects;
            }
            state.sessions = sessions;
            state.attention = attention;
            state.attention_projection_pending = attention_projection_pending;
            state.unresolved_writes = unresolved_writes;
            state.attention_selection = state
                .attention_selection
                .min(state.attention.len().saturating_sub(1));
        }
        UiEvent::SettingsLoaded(settings) => {
            if state.selected == Some(settings.session)
                && state.settings_query == settings.query
                && current_generation_mut(state, settings.session, settings.generation).is_some()
            {
                state.settings_profile = state
                    .settings_profile
                    .min(settings.profiles.len().saturating_sub(1));
                state.settings = Some(*settings);
            }
        }
        UiEvent::ConfigUpdated {
            session,
            generation,
        } => {
            if current_generation_mut(state, session, generation).is_some()
                && matches!(
                    state.dialog,
                    Some(Dialog::ModelConfig {
                        session: active_session,
                        generation: active_generation,
                        ..
                    }) if active_session == session && active_generation == generation
                )
            {
                state.dialog = None;
                state.status = Some("配置已保存并应用".into());
                request_settings(state, session, generation, &mut effects);
            }
        }
        UiEvent::ConfigUpdateFailed {
            session,
            generation,
            message,
        } => {
            if current_generation_mut(state, session, generation).is_some() {
                if let Some(Dialog::ModelConfig {
                    session: active_session,
                    generation: active_generation,
                    submitting,
                    ..
                }) = &mut state.dialog
                    && *active_session == session
                    && *active_generation == generation
                {
                    *submitting = false;
                }
                state.status = Some(message);
                request_settings(state, session, generation, &mut effects);
            }
        }
        UiEvent::LoginChanged {
            profile,
            query,
            state: login,
        } => {
            if state.login_queries.get(&profile) != Some(&query) {
                return effects;
            }
            let terminal = matches!(
                login,
                bone_app::LoginState::Succeeded
                    | bone_app::LoginState::Failed { .. }
                    | bone_app::LoginState::Cancelled
            );
            state.login_states.insert(profile.clone(), login);
            if terminal {
                state.status = Some("登录状态已更新".into());
            }
            if matches!(
                state.login_states.get(&profile),
                Some(bone_app::LoginState::Succeeded)
            ) {
                effects.push(Effect::ReloadOpenSessions {
                    profile: profile.clone(),
                    query,
                });
                if let Some(ui) = state.selected_ui() {
                    let session = ui.info.id;
                    let generation = ui.generation;
                    request_settings(state, session, generation, &mut effects);
                }
            }
        }
        UiEvent::ConfigsReloaded {
            profile,
            query,
            failures,
        } => {
            if state.login_queries.get(&profile) == Some(&query)
                && matches!(
                    state.login_states.get(&profile),
                    Some(bone_app::LoginState::Succeeded)
                )
            {
                state.status = Some(if failures == 0 {
                    "登录成功，已重新应用打开会话的配置".into()
                } else {
                    format!("登录成功；{failures} 个打开会话仍未能应用配置")
                });
            }
        }
        UiEvent::ApiKeySaved {
            session,
            generation,
            profile,
            reload_failures,
        } => {
            if current_generation_mut(state, session, generation).is_some()
                && matches!(
                    &state.dialog,
                    Some(Dialog::ApiKey {
                        session: active_session,
                        generation: active_generation,
                        profile: active_profile,
                        ..
                    }) if *active_session == session
                        && *active_generation == generation
                        && *active_profile == profile
                )
            {
                state.dialog = None;
                state.status = Some(if reload_failures == 0 {
                    "API Key 已安全保存，打开会话已重新应用配置".into()
                } else {
                    format!("API Key 已保存；{reload_failures} 个打开会话仍未能应用配置")
                });
                request_settings(state, session, generation, &mut effects);
            }
        }
        UiEvent::ApiKeyFailed {
            session,
            generation,
            profile,
            message,
        } => {
            if let Some(Dialog::ApiKey {
                session: active_session,
                generation: active_generation,
                profile: active_profile,
                submitting,
                ..
            }) = &mut state.dialog
                && *active_session == session
                && *active_generation == generation
                && *active_profile == profile
            {
                *submitting = false;
                state.status = Some(message);
            }
        }
        UiEvent::WriteResolved { session, call } => {
            if matches!(
                state.dialog,
                Some(Dialog::ResolveWrite {
                    session: active_session,
                    call: active_call,
                    ..
                }) if active_session == session && active_call == call
            ) {
                state.dialog = None;
                state.detail = None;
                state.attention_detail = None;
                state.status = Some("未知写入核查结果已保存".into());
                state.overview_pending = false;
                effects.extend(update(state, UiEvent::RefreshOverviewRequested));
            }
        }
        UiEvent::WriteResolutionFailed {
            session,
            call,
            message,
        } => {
            if let Some(Dialog::ResolveWrite {
                session: active_session,
                call: active_call,
                submitting,
                ..
            }) = &mut state.dialog
                && *active_session == session
                && *active_call == call
            {
                *submitting = false;
                state.status = Some(message);
            }
        }
        UiEvent::ResultsLoaded {
            session,
            generation,
            query,
            page,
        } => {
            if current_generation_mut(state, session, generation)
                .is_some_and(|ui| ui.result_query == query)
            {
                if state
                    .results
                    .get(&session)
                    .is_some_and(|current| current.snapshot_through != page.snapshot_through)
                {
                    if let Some(ui) = state.session_ui.get_mut(&session) {
                        ui.result_loading = false;
                        ui.result_pages.fail();
                    }
                    state.status = Some("结果分页期间已有新版本，正在重新读取".into());
                    request_results(state, session, &mut effects);
                    return effects;
                }
                if let Some(ui) = state.session_ui.get_mut(&session) {
                    ui.result_loading = false;
                    ui.result_confirmed_query = query;
                    ui.results_window_stale = false;
                    ui.result_pages.finish();
                }
                let newest = page.items.last().map(|item| item.result);
                state.results.insert(session, page);
                if let Some(result) = newest {
                    request_acceptances(state, session, result, None, false, &mut effects);
                    if state
                        .detail
                        .as_ref()
                        .is_some_and(|detail| detail.kind == DetailKind::Artifacts)
                        && state.selected == Some(session)
                    {
                        request_artifact(state, result, &mut effects);
                    }
                }
            }
        }
        UiEvent::OlderResultsLoaded {
            session,
            generation,
            query,
            page,
        } => {
            if let Some(ui) = current_generation_mut(state, session, generation) {
                ui.results_older_loading = false;
            }
            if current_generation_mut(state, session, generation)
                .is_some_and(|ui| ui.result_query == query)
            {
                if let Some(ui) = state.session_ui.get_mut(&session) {
                    ui.result_pages.finish();
                }
                state.results.insert(session, page);
                state.detail_scroll = 0;
            }
        }
        UiEvent::AcceptancesLoaded {
            session,
            generation,
            result,
            query,
            page,
            append_older,
        } => {
            state.acceptance_loading.remove(&result);
            if current_generation_mut(state, session, generation).is_some()
                && state.acceptance_queries.get(&result) == Some(&query)
            {
                state.acceptance_pages.entry(result).or_default().finish();
                state.acceptances.insert(result, page);
                state.detail_scroll = 0;
                if !append_older {
                    state.acceptance_windows_stale.remove(&result);
                }
            }
        }
        UiEvent::AcceptanceSubmitted {
            session,
            generation,
            request_id,
            receipt: _,
        } => {
            if current_generation_mut(state, session, generation).is_some()
                && matches!(
                    &state.dialog,
                    Some(Dialog::Acceptance {
                        request_id: active,
                        session: active_session,
                        generation: active_generation,
                        ..
                    }) if *active == request_id
                        && *active_session == session
                        && *active_generation == generation
                )
            {
                state.dialog = None;
                state.status = Some("验收判定已保存".into());
                request_results(state, session, &mut effects);
            }
        }
        UiEvent::AcceptanceFailed {
            session,
            generation,
            request_id,
            message,
        } => {
            if current_generation_mut(state, session, generation).is_some()
                && let Some(Dialog::Acceptance {
                    request_id: active,
                    session: active_session,
                    generation: active_generation,
                    submitting,
                    ..
                }) = &mut state.dialog
                && *active == request_id
                && *active_session == session
                && *active_generation == generation
            {
                *submitting = false;
                state.status = Some(message);
            }
        }
        UiEvent::WorkspaceChangesLoaded {
            workspace,
            query,
            page,
            append,
        } => {
            if state.workspace.as_ref().map(|(id, _)| *id) != Some(workspace)
                || state.workspace_changes.query != query
            {
                return effects;
            }
            state.workspace_changes.loading = false;
            if append
                && state
                    .workspace_changes
                    .page
                    .as_ref()
                    .is_some_and(|current| current.baseline != page.baseline)
            {
                let baseline_matches = state
                    .workspace_changes
                    .page
                    .as_ref()
                    .is_some_and(|current| current.baseline == page.baseline);
                if !baseline_matches {
                    state.workspace_changes.pages.fail();
                    state.workspace_changes.page = None;
                    clear_workspace_file(state);
                    state.status = Some("读取期间 Git HEAD 已变化，正在重新加载变更".into());
                    request_workspace_changes(state, false, &mut effects);
                    return effects;
                }
            }
            state.workspace_changes.pages.finish();
            state.workspace_changes.page = Some(page);
            state.workspace_changes.selection = 0;
            clear_workspace_file(state);
        }
        UiEvent::WorkspaceFileLoaded {
            workspace,
            query,
            cursor,
            append,
            page: file,
        } => {
            if state.workspace.as_ref().map(|(id, _)| *id) != Some(workspace)
                || state.workspace_changes.file_query != query
                || !state.workspace_changes.file_loading
                || state.workspace_changes.file_request_cursor != cursor
                || state.workspace_changes.file_request_append != append
                || state.workspace_changes.file_request_path.as_deref() != Some(&file.path)
                || state.workspace_changes.file_request_source != Some(file.source)
            {
                return effects;
            }
            state.workspace_changes.file_loading = false;
            state
                .workspace_changes
                .file_layout
                .borrow_mut()
                .lines
                .clear();
            let is_current = state.workspace_changes.page.as_ref().is_some_and(|page| {
                page.baseline == file.baseline
                    && page.files.iter().any(|changed| changed.path == file.path)
            });
            if is_current {
                if append {
                    if state.workspace_changes.file_back.len() == 8 {
                        state.workspace_changes.file_back.remove(0);
                    }
                    let previous_cursor = state.workspace_changes.file_cursor.clone();
                    state.workspace_changes.file_back.push(previous_cursor);
                } else if state.workspace_changes.file_back.last() == Some(&cursor) {
                    state.workspace_changes.file_back.pop();
                }
                state.workspace_changes.file_cursor = cursor;
                state.workspace_changes.file = Some(file);
                state.detail_scroll = 0;
                state.workspace_changes.file_control_selection =
                    preferred_workspace_file_control(state);
            } else {
                state.status = Some("该文件的 Git 基线已变化，正在重新加载变更".into());
                clear_workspace_file(state);
                request_workspace_changes(state, false, &mut effects);
            }
        }
        UiEvent::WorkspaceChangesFailed {
            workspace,
            query,
            message,
        } => {
            if state.workspace.as_ref().map(|(id, _)| *id) == Some(workspace)
                && state.workspace_changes.query == query
            {
                state.workspace_changes.loading = false;
                state.workspace_changes.pages.fail();
                state.status = Some(message);
            }
        }
        UiEvent::WorkspaceFileFailed {
            workspace,
            query,
            path,
            source,
            cursor,
            append,
            message,
        } => {
            if state.workspace.as_ref().map(|(id, _)| *id) == Some(workspace)
                && state.workspace_changes.file_query == query
                && state.workspace_changes.file_loading
                && state.workspace_changes.file_request_cursor == cursor
                && state.workspace_changes.file_request_append == append
                && state.workspace_changes.file_request_path.as_deref() == Some(&path)
                && state.workspace_changes.file_request_source == Some(source)
            {
                state.workspace_changes.file_loading = false;
                if cursor.is_some() || state.workspace_changes.file.is_some() {
                    clear_workspace_file(state);
                    state.status = Some(format!(
                        "文件或 Git 基线在分页期间已变化，正在刷新文件列表：{message}"
                    ));
                    request_workspace_changes(state, false, &mut effects);
                } else {
                    state.status = Some(message);
                }
            }
        }
        UiEvent::ArtifactLoaded {
            result,
            query,
            artifact,
            evidence,
        } => {
            if state.selected == Some(result.session)
                && state.artifact.query == query
                && artifact.result == result
                && evidence.result == result
            {
                state.artifact.loading = false;
                state.artifact.artifact = Some(artifact);
                state.artifact.evidence = Some(evidence);
                state.artifact.evidence_pages = PageNavigation::default();
                state.artifact.selection = 0;
                state.artifact.source = None;
                state.artifact.source_loading = false;
            }
        }
        UiEvent::EvidenceLoaded {
            result,
            query,
            page,
        } => {
            if state.selected == Some(result.session)
                && state.artifact.query == query
                && state
                    .artifact
                    .artifact
                    .as_ref()
                    .is_some_and(|artifact| artifact.result == result)
                && page.result == result
            {
                state.artifact.loading = false;
                state.artifact.evidence_pages.finish();
                state.artifact.evidence = Some(page);
                state.artifact.selection = 0;
            }
        }
        UiEvent::EvidenceSourceLoaded {
            result,
            query,
            page,
            append,
        } => {
            if state.selected != Some(result.session)
                || state.artifact.source_query != query
                || !state
                    .artifact
                    .artifact
                    .as_ref()
                    .is_some_and(|artifact| artifact.result == result)
            {
                return effects;
            }
            state.artifact.source_loading = false;
            if append {
                if let Some(reader) = state.artifact.source.as_mut()
                    && reader.result == result
                    && reader.source == page.source
                    && reader.next_offset == Some(page.offset)
                {
                    if let Some(text) = page.text {
                        reader.text.push_str(&text);
                        if reader.text.len() > EVIDENCE_VIEW_BYTES {
                            let overflow = reader.text.len() - EVIDENCE_VIEW_BYTES;
                            let mut drop = overflow;
                            while drop < reader.text.len() && !reader.text.is_char_boundary(drop) {
                                drop += 1;
                            }
                            reader.text.drain(..drop);
                            reader.window_offset = reader.window_offset.saturating_add(drop as u64);
                            state.detail_scroll = 0;
                        }
                    }
                    reader.next_offset = page.next_offset;
                    reader.total_bytes = page.total_bytes;
                    reader.projection_pending = page.projection_pending;
                    reader.frontend_truncated =
                        reader.window_offset > 0 || reader.next_offset.is_some();
                    reader.layout.borrow_mut().lines.clear();
                }
            } else {
                let mut text = page.text.unwrap_or_default();
                let mut window_offset = page.offset;
                if text.len() > EVIDENCE_VIEW_BYTES {
                    let overflow = text.len() - EVIDENCE_VIEW_BYTES;
                    let mut drop = overflow;
                    while drop < text.len() && !text.is_char_boundary(drop) {
                        drop += 1;
                    }
                    text.drain(..drop);
                    window_offset = window_offset.saturating_add(drop as u64);
                }
                let frontend_truncated = window_offset > 0 || page.next_offset.is_some();
                state.artifact.source = Some(EvidenceReaderUi {
                    result,
                    source: page.source,
                    availability: page.availability,
                    text,
                    window_offset,
                    next_offset: page.next_offset,
                    total_bytes: page.total_bytes,
                    projection_pending: page.projection_pending,
                    frontend_truncated,
                    layout: Default::default(),
                });
                state.detail_scroll = 0;
            }
        }
        UiEvent::ArtifactFailed {
            result,
            query,
            message,
        } => {
            if state.selected == Some(result.session) && state.artifact.query == query {
                state.artifact.loading = false;
                state.artifact.evidence_pages.fail();
                state.status = Some(message);
            }
        }
        UiEvent::EvidenceSourceFailed {
            result,
            query,
            message,
        } => {
            if state.selected == Some(result.session)
                && state.artifact.source_query == query
                && state
                    .artifact
                    .artifact
                    .as_ref()
                    .is_some_and(|artifact| artifact.result == result)
            {
                state.artifact.source_loading = false;
                state.status = Some(message);
            }
        }
        UiEvent::DraftSaved {
            session,
            generation,
            revision,
            text,
        } => {
            if let Some(ui) = current_generation_mut(state, session, generation) {
                if revision >= ui.saved_draft_revision {
                    ui.saved_draft_revision = revision;
                    ui.saved_draft = text;
                }
                if ui.release_requested && state.selected != Some(session) {
                    effects.push(Effect::ReleaseSession {
                        session,
                        generation,
                    });
                }
            }
        }
        UiEvent::Submitted {
            session,
            generation,
            request_id,
            receipt: _,
        } => {
            if let Some(ui) = current_generation_mut(state, session, generation)
                && ui
                    .submitting
                    .as_ref()
                    .is_some_and(|pending| pending.request_id == request_id)
            {
                let pending = ui.submitting.take().expect("pending submission exists");
                if pending.clear_on_receipt && ui.draft.starts_with(&pending.text) {
                    ui.draft.drain(..pending.text.len());
                    ui.draft_revision = ui.draft_revision.wrapping_add(1);
                }
                if ui.reply_to == pending.reply_to {
                    ui.reply_to = None;
                }
                ui.queued_draft_revision = ui.draft_revision;
                effects.push(Effect::SaveDraft {
                    session,
                    generation,
                    revision: ui.draft_revision,
                    text: ui.draft.clone(),
                });
            }
        }
        UiEvent::SessionRenamed {
            session,
            generation,
            operation,
            title,
        } => {
            if session_management_response_is_current(state, session, generation, operation) {
                update_session_info(state, session, |info| info.title = title.clone());
                state.session_management_operations.remove(&session);
                if matches!(
                    &state.dialog,
                    Some(Dialog::RenameSession {
                        session: active_session,
                        generation: active_generation,
                        operation: active_operation,
                        ..
                    }) if *active_session == session
                        && *active_generation == generation
                        && *active_operation == operation
                ) {
                    state.dialog = None;
                }
                state.status = Some("会话名称已更新".into());
            }
        }
        UiEvent::SessionArchived {
            session,
            generation,
            operation,
            archived,
        } => {
            if session_management_response_is_current(state, session, generation, operation) {
                update_session_info(state, session, |info| info.archived = archived);
                state.session_management_operations.remove(&session);
                state.status = Some(if archived {
                    "会话已归档".into()
                } else {
                    "会话已恢复".into()
                });
            }
        }
        UiEvent::SessionManagementFailed {
            session,
            generation,
            operation,
            message,
        } => {
            if session_management_response_is_current(state, session, generation, operation) {
                state.session_management_operations.remove(&session);
                if let Some(Dialog::RenameSession {
                    session: active_session,
                    generation: active_generation,
                    operation: active_operation,
                    submitting,
                    ..
                }) = &mut state.dialog
                    && *active_session == session
                    && *active_generation == generation
                    && *active_operation == operation
                {
                    *submitting = false;
                }
                state.status = Some(message);
            }
        }
        UiEvent::SessionCreated(info) => {
            if !state.sessions.iter().any(|item| item.id == info.id) {
                state.sessions.push(info.clone());
            }
            select_session(state, info.id, &mut effects);
        }
        UiEvent::OperationFailed {
            kind,
            session,
            generation,
            message,
        } => {
            if session.is_none_or(|id| {
                generation.is_none_or(|generation| {
                    state
                        .session_ui
                        .get(&id)
                        .is_some_and(|ui| ui.generation == generation)
                })
            }) {
                if kind == OperationKind::RefreshOverview {
                    state.overview_pending = false;
                }
                if kind == OperationKind::LoadWorkspaceChanges {
                    state.workspace_changes.loading = false;
                }
                if kind == OperationKind::LoadWorkspaceFile {
                    state.workspace_changes.file_loading = false;
                }
                if let (Some(id), Some(generation)) = (session, generation)
                    && let Some(ui) = current_generation_mut(state, id, generation)
                {
                    match kind {
                        OperationKind::Submit => {
                            if let Some(pending) = &mut ui.submitting {
                                pending.failed = true;
                            }
                        }
                        OperationKind::LoadHistory => ui.history_loading = false,
                        OperationKind::LoadOlderResults => {
                            ui.results_older_loading = false;
                            ui.result_pages.fail();
                        }
                        OperationKind::LoadOlderHistory => ui.older_loading = false,
                        OperationKind::ReloadRecentHistory => ui.recent_reloading = false,
                        OperationKind::LoadResults => {
                            ui.result_loading = false;
                            ui.result_pages.fail();
                        }
                        OperationKind::SaveDraft => {
                            ui.queued_draft_revision = ui.saved_draft_revision;
                        }
                        _ => {}
                    }
                }
                if kind == OperationKind::LoadAcceptances
                    && let Some(id) = session
                {
                    state
                        .acceptance_loading
                        .retain(|result| result.session != id);
                    for (result, pages) in &mut state.acceptance_pages {
                        if result.session == id {
                            pages.fail();
                        }
                    }
                }
                state.status = Some(message);
            }
        }
        UiEvent::Resized => {}
        UiEvent::PersistDraftsRequested => {
            unreachable!("draft persistence returns before dispatch")
        }
        UiEvent::Tick => unreachable!("ticks return before dispatch"),
    }
    enforce_history_budget(state);
    state.dirty = true;
    effects
}

fn handle_action(state: &mut UiState, action: Action, effects: &mut Vec<Effect>) {
    if state.dialog.is_some() {
        match action {
            Action::Escape
                if matches!(
                    &state.dialog,
                    Some(Dialog::Acceptance {
                        submitting: true,
                        ..
                    }) | Some(Dialog::RenameSession {
                        submitting: true,
                        ..
                    }) | Some(Dialog::ModelConfig {
                        submitting: true,
                        ..
                    }) | Some(Dialog::ResolveWrite {
                        submitting: true,
                        ..
                    }) | Some(Dialog::ApiKey {
                        submitting: true,
                        ..
                    })
                ) =>
            {
                state.status = Some("正在等待 App 确认保存结果".into());
            }
            Action::Escape => state.dialog = None,
            Action::Input(value) => match &mut state.dialog {
                Some(Dialog::NewSession { title }) => title.push(value),
                Some(Dialog::RenameSession { title, .. }) if !value.is_control() => {
                    title.push(value);
                }
                Some(Dialog::Acceptance {
                    reason,
                    rework,
                    editing_rework,
                    submitting: false,
                    ..
                }) => {
                    if *editing_rework {
                        rework.push(value);
                    } else {
                        reason.push(value);
                    }
                }
                Some(Dialog::ModelConfig { model, .. }) if !value.is_control() => model.push(value),
                Some(Dialog::ApiKey {
                    key,
                    submitting: false,
                    ..
                }) if !value.is_control() => key.push(value),
                Some(Dialog::ResolveWrite {
                    evidence,
                    submitting: false,
                    ..
                }) => evidence.push(value),
                _ => {}
            },
            Action::Paste(value) => match &mut state.dialog {
                Some(Dialog::NewSession { title }) => {
                    title.push_str(&value.replace(['\r', '\n', '\t'], " "));
                }
                Some(Dialog::RenameSession { title, .. }) => {
                    title.extend(value.chars().map(
                        |value| {
                            if value.is_control() { ' ' } else { value }
                        },
                    ));
                }
                Some(Dialog::Acceptance {
                    reason,
                    rework,
                    editing_rework,
                    submitting: false,
                    ..
                }) => {
                    if *editing_rework {
                        rework.push_str(&value);
                    } else {
                        reason.push_str(&value);
                    }
                }
                Some(Dialog::ModelConfig { model, .. }) => {
                    model.extend(value.chars().filter(|value| !value.is_control()));
                }
                Some(Dialog::ApiKey {
                    key,
                    submitting: false,
                    ..
                }) => key.extend(value.chars().filter(|value| !value.is_control())),
                Some(Dialog::ResolveWrite {
                    evidence,
                    submitting: false,
                    ..
                }) => evidence.push_str(&value),
                _ => {}
            },
            Action::Backspace => match &mut state.dialog {
                Some(Dialog::NewSession { title }) => remove_last_grapheme(title),
                Some(Dialog::RenameSession { title, .. }) => remove_last_grapheme(title),
                Some(Dialog::Acceptance {
                    reason,
                    rework,
                    editing_rework,
                    submitting: false,
                    ..
                }) => remove_last_grapheme(if *editing_rework { rework } else { reason }),
                Some(Dialog::ModelConfig { model, .. }) => remove_last_grapheme(model),
                Some(Dialog::ApiKey {
                    key,
                    submitting: false,
                    ..
                }) => remove_last_grapheme(key.as_mut_string()),
                Some(Dialog::ResolveWrite {
                    evidence,
                    submitting: false,
                    ..
                }) => remove_last_grapheme(evidence),
                _ => {}
            },
            Action::FocusNext | Action::FocusPrevious => {
                if let Some(Dialog::Acceptance {
                    decision: bone_app::AcceptanceDecision::Rejected,
                    editing_rework,
                    submitting: false,
                    ..
                }) = &mut state.dialog
                {
                    *editing_rework = !*editing_rework;
                }
            }
            Action::Submit | Action::Activate => match &state.dialog {
                Some(Dialog::ConfirmQuit) => {
                    state.quitting = true;
                    effects.push(Effect::Shutdown);
                }
                Some(Dialog::NewSession { title }) => {
                    let title = title.trim();
                    if !title.is_empty() {
                        effects.push(Effect::CreateSession {
                            title: title.to_owned(),
                        });
                        state.dialog = None;
                    }
                }
                Some(Dialog::RenameSession {
                    session,
                    generation,
                    operation,
                    title,
                    submitting,
                }) if !*submitting => {
                    let title = title.trim();
                    if title.is_empty() {
                        state.status = Some("会话名称不能为空".into());
                    } else {
                        effects.push(Effect::RenameSession {
                            session: *session,
                            generation: *generation,
                            operation: *operation,
                            title: title.to_owned(),
                        });
                        state
                            .session_management_operations
                            .insert(*session, *operation);
                        if let Some(Dialog::RenameSession { submitting, .. }) = &mut state.dialog {
                            *submitting = true;
                        }
                    }
                }
                Some(Dialog::RenameSession { .. }) => {}
                Some(Dialog::Error(_)) => state.dialog = None,
                Some(Dialog::Acceptance {
                    session,
                    generation,
                    request_id,
                    rework_request_id,
                    result,
                    decision,
                    reason,
                    rework,
                    submitting,
                    ..
                }) if !*submitting => {
                    let reason_required = *decision != bone_app::AcceptanceDecision::Accepted;
                    let rework_required = *decision == bone_app::AcceptanceDecision::Rejected;
                    if (reason_required && reason.trim().is_empty())
                        || (rework_required && rework.trim().is_empty())
                    {
                        state.status = Some("请补全验收理由和返工要求".into());
                    } else {
                        let rework_request_id = *rework_request_id;
                        effects.push(Effect::SubmitAcceptance {
                            session: *session,
                            generation: *generation,
                            submission: bone_app::AcceptanceSubmission {
                                request_id: *request_id,
                                result: *result,
                                decision: *decision,
                                reason: reason.clone(),
                                rework: rework_required.then(|| {
                                    let mut input = SubmitInput::new(rework.clone());
                                    input.request_id = rework_request_id
                                        .expect("rejected acceptance owns a stable request id");
                                    input
                                }),
                            },
                        });
                        if let Some(Dialog::Acceptance { submitting, .. }) = &mut state.dialog {
                            *submitting = true;
                        }
                    }
                }
                Some(Dialog::Acceptance { .. }) => {}
                Some(Dialog::ModelConfig {
                    session,
                    generation,
                    role,
                    profile,
                    model,
                    submitting,
                }) if !*submitting => {
                    match bone_app::ModelSelection::new(profile.clone(), model.trim().to_owned()) {
                        Ok(selection) => {
                            let change = match role {
                                ModelRole::Worker => {
                                    bone_app::ConfigChange::Worker(Some(selection))
                                }
                                ModelRole::Coordinator => {
                                    bone_app::ConfigChange::Coordinator(Some(selection))
                                }
                            };
                            effects.push(Effect::UpdateConfig {
                                session: *session,
                                generation: *generation,
                                change,
                            });
                            if let Some(Dialog::ModelConfig { submitting, .. }) = &mut state.dialog
                            {
                                *submitting = true;
                            }
                        }
                        Err(error) => state.status = Some(format!("模型配置无效：{error}")),
                    }
                }
                Some(Dialog::ModelConfig { .. }) => {}
                Some(Dialog::ApiKey {
                    session,
                    generation,
                    profile,
                    key,
                    submitting,
                }) if !*submitting => {
                    if key.is_blank() {
                        state.status = Some("API Key 不能为空".into());
                    } else {
                        effects.push(Effect::SetApiKey {
                            session: *session,
                            generation: *generation,
                            profile: profile.clone(),
                            key: key.clone(),
                        });
                        if let Some(Dialog::ApiKey { submitting, .. }) = &mut state.dialog {
                            *submitting = true;
                        }
                    }
                }
                Some(Dialog::ApiKey { .. }) => {}
                Some(Dialog::ResolveWrite {
                    session,
                    call,
                    external_effect,
                    evidence,
                    submitting,
                }) if !*submitting => {
                    if evidence.trim().is_empty() {
                        state.status = Some("请填写核查依据".into());
                    } else {
                        effects.push(Effect::ResolveWrite {
                            session: *session,
                            call: *call,
                            resolution: bone_app::WriteResolution {
                                external_effect: *external_effect,
                                evidence: evidence.clone(),
                            },
                        });
                        if let Some(Dialog::ResolveWrite { submitting, .. }) = &mut state.dialog {
                            *submitting = true;
                        }
                    }
                }
                Some(Dialog::ResolveWrite { .. }) => {}
                None => {}
            },
            Action::Quit => state.dialog = Some(Dialog::ConfirmQuit),
            Action::Terminate => {
                state.quitting = true;
                effects.push(Effect::Shutdown);
            }
            _ => {}
        }
        return;
    }

    match action {
        Action::Noop => {}
        Action::FocusNext => state.focus = next_focus(state.focus, false),
        Action::FocusPrevious => state.focus = next_focus(state.focus, true),
        Action::MoveFocusedControl(delta) => move_focused_control(state, delta),
        Action::FocusVisibleControl(target) => state.focused_control = Some(target),
        Action::SelectPrevious => move_selection(state, -1, effects),
        Action::SelectNext => move_selection(state, 1, effects),
        Action::SelectSession(id) => {
            let remain_in_browser = state.main == MainView::Sessions;
            select_session(state, id, effects);
            if remain_in_browser {
                state.main = MainView::Sessions;
                state.focus = Focus::Timeline;
            }
        }
        Action::Activate => {
            if state.focus == Focus::Global {
                let target = main_view_at(state.global_selection);
                handle_action(state, Action::Open(target), effects);
            } else if state.focus == Focus::Actions {
                let action = selected_action_bar_action(state);
                handle_action(state, action, effects);
            } else if state.focus == Focus::Detail
                && state
                    .detail
                    .as_ref()
                    .is_some_and(|detail| detail.kind != DetailKind::Decision)
                && state
                    .detail
                    .as_ref()
                    .is_some_and(|detail| detail.kind != detail_kind_at(state.detail_tab_selection))
            {
                handle_action(
                    state,
                    Action::OpenDetail(detail_state_at(state.detail_tab_selection)),
                    effects,
                );
            } else if state.main == MainView::Attention && state.detail.is_none() {
                handle_action(
                    state,
                    Action::OpenAttention(state.attention_selection),
                    effects,
                );
            } else if state
                .detail
                .as_ref()
                .is_some_and(|detail| detail.kind == DetailKind::Decision)
                && state.focus == Focus::Detail
            {
                handle_action(
                    state,
                    Action::BeginWriteResolution(if state.write_resolution_applied {
                        bone_app::ExternalEffect::Applied
                    } else {
                        bone_app::ExternalEffect::None
                    }),
                    effects,
                );
            } else if state
                .detail
                .as_ref()
                .is_some_and(|detail| detail.kind == DetailKind::Changes)
                && state.workspace_changes.file_loading
            {
                handle_action(state, Action::CloseWorkspaceFile, effects);
            } else if state
                .detail
                .as_ref()
                .is_some_and(|detail| detail.kind == DetailKind::Changes)
                && state.workspace_changes.file.is_some()
            {
                let action = selected_workspace_file_action(state);
                handle_action(state, action, effects);
            } else if state
                .detail
                .as_ref()
                .is_some_and(|detail| detail.kind == DetailKind::Changes)
                && state.workspace_changes.file.is_none()
            {
                handle_action(
                    state,
                    Action::OpenWorkspaceChange(state.workspace_changes.selection),
                    effects,
                );
            } else if state
                .detail
                .as_ref()
                .is_some_and(|detail| detail.kind == DetailKind::Artifacts)
                && state.artifact.source.is_none()
                && !state.artifact.source_loading
            {
                handle_action(
                    state,
                    Action::OpenEvidence(state.artifact.selection),
                    effects,
                );
            }
        }
        Action::Escape => {
            if state
                .detail
                .as_ref()
                .is_some_and(|detail| detail.kind == DetailKind::Changes)
                && (state.workspace_changes.file.is_some() || state.workspace_changes.file_loading)
            {
                clear_workspace_file(state);
                state.workspace_changes.file_query = state.generation();
                state.detail_scroll = 0;
                state.focus = Focus::Detail;
            } else if state
                .detail
                .as_ref()
                .is_some_and(|detail| detail.kind == DetailKind::Artifacts)
                && (state.artifact.source.is_some() || state.artifact.source_loading)
            {
                state.artifact.source = None;
                state.artifact.source_loading = false;
                state.artifact.source_query = state.generation();
                state.detail_scroll = 0;
                state.focus = Focus::Detail;
            } else if state.detail.take().is_some() {
                state.detail_scroll = 0;
                state.focus = Focus::Timeline;
            } else if state.main != MainView::Workbench {
                state.main = MainView::Workbench;
                state.focus = Focus::Composer;
            }
        }
        Action::Open(main) => {
            state.main = main;
            state.global_selection = main_view_index(main);
            state.action_selection = 0;
            state.focused_control = None;
            state.detail = None;
            state.detail_scroll = 0;
            state.focus = Focus::Timeline;
            if main == MainView::Settings
                && let Some(ui) = state.selected_ui()
            {
                let session = ui.info.id;
                let generation = ui.generation;
                request_settings(state, session, generation, effects);
            }
        }
        Action::OpenDetail(detail) => {
            let load_results = detail.kind == DetailKind::Acceptance;
            let load_artifact = detail.kind == DetailKind::Artifacts;
            let load_changes = detail.kind == DetailKind::Changes;
            state.detail_tab_selection = detail_kind_index(detail.kind);
            state.focused_control = None;
            state.detail = Some(detail);
            state.detail_scroll = 0;
            state.focus = Focus::Detail;
            if load_results && let Some(session) = state.selected {
                request_results(state, session, effects);
            }
            if load_artifact && let Some(session) = state.selected {
                state.artifact = ArtifactUi::default();
                request_results(state, session, effects);
            }
            if load_changes {
                request_workspace_changes(state, false, effects);
            }
        }
        Action::CloseDetail => {
            state.detail = None;
            state.detail_scroll = 0;
            state.focused_control = None;
            state.focus = Focus::Timeline;
        }
        Action::Focus(focus) => state.focus = focus,
        Action::NewSession => {
            state.dialog = Some(Dialog::NewSession {
                title: String::new(),
            });
            state.focus = Focus::Dialog;
        }
        Action::BeginRenameSession(session) => {
            if state.selected == Some(session)
                && let Some(ui) = state.session_ui.get(&session)
            {
                let generation = ui.generation;
                let title = ui.info.title.clone();
                let operation = state.session_management_operation();
                state.dialog = Some(Dialog::RenameSession {
                    session,
                    generation,
                    operation,
                    title,
                    submitting: false,
                });
                state.focus = Focus::Dialog;
            }
        }
        Action::SetSessionArchived { session, archived } => {
            if state.selected == Some(session)
                && let Some(ui) = state.session_ui.get(&session)
                && !state.session_management_operations.contains_key(&session)
            {
                let generation = ui.generation;
                let operation = state.session_management_operation();
                state
                    .session_management_operations
                    .insert(session, operation);
                effects.push(Effect::ArchiveSession {
                    session,
                    generation,
                    operation,
                    archived,
                });
                state.status = Some(if archived {
                    "正在归档会话…".into()
                } else {
                    "正在恢复会话…".into()
                });
            }
        }
        Action::Input(value) => edit_selected(state, |ui| ui.draft.push(value), effects),
        Action::Paste(text) => edit_selected(state, |ui| ui.draft.push_str(&text), effects),
        Action::Backspace => edit_selected(
            state,
            |ui| {
                remove_last_grapheme(&mut ui.draft);
            },
            effects,
        ),
        Action::InsertNewline => edit_selected(state, |ui| ui.draft.push('\n'), effects),
        Action::Submit if state.focus == Focus::Composer => submit_selected(state, effects),
        Action::Submit => {}
        Action::SubmitFromActionBar => submit_selected(state, effects),
        Action::ScrollUp(amount) => {
            if state.focus == Focus::Detail && state.detail.is_some() {
                state.detail_scroll = state.detail_scroll.saturating_sub(amount);
                return;
            }
            if let Some(ui) = state.selected_ui_mut() {
                if ui.history_window_stale && !ui.recent_reloading {
                    ui.recent_reloading = true;
                    effects.push(Effect::ReloadRecentHistory {
                        session: ui.info.id,
                        generation: ui.generation,
                    });
                    return;
                }
                ui.scroll_from_tail = ui
                    .scroll_from_tail
                    .saturating_add(amount)
                    .min(ui.history.len().saturating_sub(1));
                if !ui.older_loading
                    && ui.scroll_from_tail.saturating_add(10) >= ui.history.len()
                    && let Some(cursor) = ui.older_cursor
                {
                    ui.older_loading = true;
                    effects.push(Effect::LoadOlderHistory {
                        session: ui.info.id,
                        generation: ui.generation,
                        cursor,
                    });
                }
            }
        }
        Action::ScrollDown(amount) => {
            if state.focus == Focus::Detail && state.detail.is_some() {
                state.detail_scroll = state.detail_scroll.saturating_add(amount);
                if state
                    .detail
                    .as_ref()
                    .is_some_and(|detail| detail.kind == DetailKind::Records)
                    && let Some(ui) = state.selected_ui_mut()
                    && !ui.older_loading
                    && let Some(cursor) = ui.older_cursor
                {
                    ui.older_loading = true;
                    effects.push(Effect::LoadOlderHistory {
                        session: ui.info.id,
                        generation: ui.generation,
                        cursor,
                    });
                }
                return;
            }
            if let Some(ui) = state.selected_ui_mut() {
                ui.scroll_from_tail = ui.scroll_from_tail.saturating_sub(amount);
                if ui.scroll_from_tail == 0 && ui.newer_history_missing && !ui.recent_reloading {
                    ui.recent_reloading = true;
                    effects.push(Effect::ReloadRecentHistory {
                        session: ui.info.id,
                        generation: ui.generation,
                    });
                }
            }
        }
        Action::Stop => {
            if let Some(ui) = state.selected_ui() {
                effects.push(Effect::Stop {
                    session: ui.info.id,
                    generation: ui.generation,
                });
            }
        }
        Action::Quit => state.dialog = Some(Dialog::ConfirmQuit),
        Action::Terminate => {
            state.quitting = true;
            effects.push(Effect::Shutdown);
        }
        Action::BeginAcceptance { decision, result } => {
            let result_is_current = state.detail_scroll == 0
                && state.selected.is_some_and(|session| {
                    state.session_ui.get(&session).is_some_and(|ui| {
                        !ui.result_loading
                            && ui.result_confirmed_query == ui.result_query
                            && ui.result_pages.current.is_none()
                    }) && state
                        .results
                        .get(&session)
                        .and_then(|page| page.items.last())
                        .is_some_and(|summary| summary.result == result)
                });
            if result_is_current && let Some(ui) = state.selected_ui() {
                state.dialog = Some(Dialog::Acceptance {
                    session: ui.info.id,
                    generation: ui.generation,
                    request_id: bone_app::AcceptanceRequestId::new(),
                    rework_request_id: (decision == bone_app::AcceptanceDecision::Rejected)
                        .then(RequestId::new),
                    result,
                    decision,
                    reason: String::new(),
                    rework: String::new(),
                    editing_rework: false,
                    submitting: false,
                });
                state.focus = Focus::Dialog;
            }
        }
        Action::LoadOlderResults => {
            if let Some(session) = state.selected {
                if state
                    .session_ui
                    .get(&session)
                    .is_some_and(|ui| ui.results_window_stale)
                {
                    request_results(state, session, effects);
                    return;
                }
                let request = state
                    .results
                    .get(&session)
                    .map(|page| (page.older_cursor, page.projection_pending));
                match request {
                    Some((Some(cursor), _)) => {
                        if let Some(ui) = state
                            .session_ui
                            .get_mut(&session)
                            .filter(|ui| !ui.results_older_loading)
                        {
                            if !ui.result_pages.begin(Some(cursor), PageDirection::Forward) {
                                return;
                            }
                            ui.results_older_loading = true;
                            effects.push(Effect::LoadOlderResults {
                                session,
                                generation: ui.generation,
                                query: ui.result_query,
                                cursor,
                            });
                        }
                    }
                    Some((None, true)) => request_results(state, session, effects),
                    _ => {}
                }
            }
        }
        Action::LoadNewerResults => request_previous_results(state, effects),
        Action::LoadOlderAcceptances => {
            if let Some(session) = state.selected
                && let Some(result) = state
                    .results
                    .get(&session)
                    .and_then(|page| page.items.last())
                    .map(|item| item.result)
            {
                if state.acceptance_windows_stale.contains(&result) {
                    request_acceptances(state, session, result, None, false, effects);
                } else if let Some(cursor) = state
                    .acceptances
                    .get(&result)
                    .and_then(|page| page.older_cursor)
                {
                    request_acceptances_page(
                        state,
                        session,
                        result,
                        Some(cursor),
                        PageDirection::Forward,
                        effects,
                    );
                }
            }
        }
        Action::LoadNewerAcceptances => request_previous_acceptances(state, effects),
        Action::SelectSettingsProfile(index) => {
            if state.settings.as_ref().is_some_and(|settings| {
                state.selected == Some(settings.session) && index < settings.profiles.len()
            }) {
                state.settings_profile = index;
            }
        }
        Action::ConfigureModel(role) => {
            if let Some(settings) = &state.settings
                && state.selected == Some(settings.session)
                && let Some(profile) = settings.profiles.get(state.settings_profile)
            {
                let model = settings
                    .resolved
                    .desired
                    .as_ref()
                    .ok()
                    .map(|config| match role {
                        ModelRole::Worker => config.worker.selection.model.clone(),
                        ModelRole::Coordinator => config.coordinator.selection.model.clone(),
                    })
                    .unwrap_or_default();
                state.dialog = Some(Dialog::ModelConfig {
                    session: settings.session,
                    generation: settings.generation,
                    role,
                    profile: profile.id.clone(),
                    model,
                    submitting: false,
                });
                state.focus = Focus::Dialog;
            }
        }
        Action::StartLogin => {
            if let Some(profile) = state
                .settings
                .as_ref()
                .filter(|settings| state.selected == Some(settings.session))
                .and_then(|settings| settings.profiles.get(state.settings_profile))
            {
                state
                    .login_states
                    .insert(profile.id.clone(), bone_app::LoginState::Connecting);
                let query = state
                    .login_queries
                    .entry(profile.id.clone())
                    .and_modify(|value| *value = value.wrapping_add(1).max(1))
                    .or_insert(1);
                effects.push(Effect::StartLogin {
                    profile: profile.id.clone(),
                    query: *query,
                });
            }
        }
        Action::ConfigureCredential => {
            if let Some(settings) = state.settings.as_ref()
                && state.selected == Some(settings.session)
                && let Some(profile) = settings.profiles.get(state.settings_profile)
            {
                if matches!(
                    &profile.endpoint,
                    bone_app::EndpointConfig::ChatGptSubscription
                ) {
                    handle_action(state, Action::StartLogin, effects);
                } else {
                    state.dialog = Some(Dialog::ApiKey {
                        session: settings.session,
                        generation: settings.generation,
                        profile: profile.id.clone(),
                        key: SecretText::default(),
                        submitting: false,
                    });
                    state.focus = Focus::Dialog;
                }
            }
        }
        Action::Logout => {
            if let Some(profile) = state
                .settings
                .as_ref()
                .filter(|settings| state.selected == Some(settings.session))
                .and_then(|settings| settings.profiles.get(state.settings_profile))
            {
                let query = state
                    .login_queries
                    .entry(profile.id.clone())
                    .and_modify(|value| *value = value.wrapping_add(1).max(1))
                    .or_insert(1);
                effects.push(Effect::Logout {
                    profile: profile.id.clone(),
                    query: *query,
                });
            }
        }
        Action::OpenAttention(index) => {
            let Some(item) = state.attention.get(index).cloned() else {
                return;
            };
            state.attention_selection = index;
            match item {
                AttentionItem::WaitingForUser {
                    session, question, ..
                } => {
                    select_session(state, session, effects);
                    if let Some(ui) = state.session_ui.get_mut(&session) {
                        ui.reply_to = Some(question);
                    }
                    state.main = MainView::Workbench;
                    state.focus = Focus::Composer;
                    state.status = Some("正在回答选中的问题；发送会绑定该问题".into());
                }
                AttentionItem::UnresolvedWrite { .. } => {
                    state.attention_detail = Some(item);
                    state.detail = Some(DetailState {
                        kind: DetailKind::Decision,
                        title: "未知写入核查".into(),
                    });
                    state.detail_scroll = 0;
                    state.write_resolution_applied = false;
                    state.focus = Focus::Detail;
                }
            }
        }
        Action::BeginWriteResolution(external_effect) => {
            if let Some(AttentionItem::UnresolvedWrite { session, call, .. }) =
                state.attention_detail.as_ref()
            {
                state.dialog = Some(Dialog::ResolveWrite {
                    session: *session,
                    call: *call,
                    external_effect,
                    evidence: String::new(),
                    submitting: false,
                });
                state.focus = Focus::Dialog;
            }
        }
        Action::MoveAttention(delta) => {
            state.attention_selection = state
                .attention_selection
                .saturating_add_signed(delta)
                .min(state.attention.len().saturating_sub(1));
        }
        Action::MoveWriteResolution(delta) => {
            if delta != 0 {
                state.write_resolution_applied = delta < 0;
            }
        }
        Action::RefreshWorkspaceChanges => request_workspace_changes(state, false, effects),
        Action::LoadOlderWorkspaceChanges => request_workspace_changes(state, true, effects),
        Action::LoadNewerWorkspaceChanges => request_previous_workspace_changes(state, effects),
        Action::MoveWorkspaceChange(delta) => {
            if state.workspace_changes.file.is_none() {
                let last = state
                    .workspace_changes
                    .page
                    .as_ref()
                    .map_or(0, |page| page.files.len().saturating_sub(1));
                state.workspace_changes.selection = state
                    .workspace_changes
                    .selection
                    .saturating_add_signed(delta)
                    .min(last);
            }
        }
        Action::OpenWorkspaceChange(index) => open_workspace_file(state, index, effects),
        Action::LoadPreviousWorkspaceFile => request_previous_workspace_file(state, effects),
        Action::LoadMoreWorkspaceFile => request_more_workspace_file(state, effects),
        Action::MoveWorkspaceFileControl(delta) => move_workspace_file_control(state, delta),
        Action::CloseWorkspaceFile => {
            clear_workspace_file(state);
            state.workspace_changes.file_query = state.generation();
            state.detail_scroll = 0;
        }
        Action::RefreshArtifact => {
            if let Some(result) = state
                .selected
                .and_then(|session| state.results.get(&session))
                .and_then(|page| page.items.last())
                .map(|summary| summary.result)
            {
                request_artifact(state, result, effects);
            } else if let Some(session) = state.selected {
                request_results(state, session, effects);
            }
        }
        Action::LoadOlderEvidence => {
            if !state.artifact.loading
                && let Some(artifact) = state.artifact.artifact.as_ref()
                && let Some(cursor) = state
                    .artifact
                    .evidence
                    .as_ref()
                    .and_then(|page| page.next_cursor)
            {
                if !state
                    .artifact
                    .evidence_pages
                    .begin(Some(cursor), PageDirection::Forward)
                {
                    return;
                }
                state.artifact.loading = true;
                effects.push(Effect::LoadEvidence {
                    result: artifact.result,
                    query: state.artifact.query,
                    cursor,
                });
            }
        }
        Action::LoadNewerEvidence => request_previous_evidence(state, effects),
        Action::MoveEvidence(delta) => {
            if state.artifact.source.is_none() {
                let last = state
                    .artifact
                    .evidence
                    .as_ref()
                    .map_or(0, |page| page.items.len().saturating_sub(1));
                state.artifact.selection = state
                    .artifact
                    .selection
                    .saturating_add_signed(delta)
                    .min(last);
            }
        }
        Action::OpenEvidence(index) => open_evidence_source(state, index, effects),
        Action::LoadPreviousEvidenceSource => load_previous_evidence_source(state, effects),
        Action::LoadMoreEvidenceSource => load_more_evidence_source(state, effects),
        Action::CloseEvidenceSource => {
            state.artifact.source = None;
            state.artifact.source_loading = false;
            state.artifact.source_query = state.generation();
            state.detail_scroll = 0;
        }
    }
}

fn select_session(state: &mut UiState, id: SessionId, effects: &mut Vec<Effect>) {
    let Some(info) = state.sessions.iter().find(|item| item.id == id).cloned() else {
        return;
    };
    let switching = state.selected != Some(id);
    if switching {
        if let Some(previous) = state.selected
            && let Some(ui) = state.session_ui.get_mut(&previous)
        {
            ui.saved_detail = state
                .detail
                .take()
                .filter(|detail| detail.kind != DetailKind::Decision);
            ui.saved_detail_scroll = state.detail_scroll;
            if ui.saved_detail.as_ref().is_some_and(|detail| {
                matches!(detail.kind, DetailKind::Artifacts | DetailKind::Acceptance)
            }) {
                // A result response that began before this navigation cannot
                // authorize acceptance or populate a restored artifact page.
                ui.result_query = ui.result_query.wrapping_add(1).max(1);
                ui.result_loading = false;
                ui.result_pages.fail();
            }
        }
        state.artifact = ArtifactUi::default();
    }
    if let Some(previous) = state.selected.filter(|previous| *previous != id)
        && let Some(ui) = state.session_ui.get_mut(&previous)
    {
        ui.release_requested = true;
        if ui.draft_revision > ui.saved_draft_revision {
            if ui.draft_revision > ui.queued_draft_revision {
                ui.queued_draft_revision = ui.draft_revision;
                effects.push(Effect::SaveDraft {
                    session: previous,
                    generation: ui.generation,
                    revision: ui.draft_revision,
                    text: ui.draft.clone(),
                });
            }
        } else {
            effects.push(Effect::ReleaseSession {
                session: previous,
                generation: ui.generation,
            });
        }
    }
    state.selected = Some(id);
    if state
        .settings
        .as_ref()
        .is_some_and(|settings| settings.session != id)
    {
        state.settings = None;
        state.settings_profile = 0;
    }
    state.main = MainView::Workbench;
    let generation = state.generation();
    let (needs_open, restored_detail, restored_scroll, session_generation) = {
        let ui = state
            .session_ui
            .entry(id)
            .or_insert_with(|| SessionUi::new(info, generation));
        ui.release_requested = false;
        ui.unread = 0;
        (
            ui.snapshot.is_none(),
            ui.saved_detail.clone(),
            ui.saved_detail_scroll,
            ui.generation,
        )
    };
    if switching {
        state.detail = restored_detail.clone();
        state.detail_scroll = restored_scroll;
        state.detail_tab_selection = restored_detail
            .as_ref()
            .map_or(0, |detail| detail_kind_index(detail.kind));
        state.focus = if restored_detail.is_some() {
            Focus::Detail
        } else {
            Focus::Composer
        };
    }
    if needs_open {
        effects.push(Effect::OpenSession {
            session: id,
            generation: session_generation,
        });
    }
    if switching && let Some(detail) = restored_detail {
        match detail.kind {
            DetailKind::Artifacts => request_results(state, id, effects),
            DetailKind::Acceptance => request_results(state, id, effects),
            DetailKind::Changes => request_workspace_changes(state, false, effects),
            DetailKind::Work | DetailKind::Context | DetailKind::Records | DetailKind::Decision => {
            }
        }
    }
}

fn request_results(state: &mut UiState, session: SessionId, effects: &mut Vec<Effect>) {
    if let Some(ui) = state.session_ui.get_mut(&session) {
        if ui.result_loading {
            return;
        }
        if !ui.result_pages.begin(None, PageDirection::Refresh) {
            return;
        }
        ui.result_query = ui.result_query.wrapping_add(1).max(1);
        ui.result_loading = true;
        effects.push(Effect::LoadResults {
            session,
            generation: ui.generation,
            query: ui.result_query,
        });
    }
}

fn request_previous_results(state: &mut UiState, effects: &mut Vec<Effect>) {
    let Some(session) = state.selected else {
        return;
    };
    let Some(ui) = state.session_ui.get_mut(&session) else {
        return;
    };
    let Some(cursor) = ui.result_pages.back.last().cloned() else {
        return;
    };
    if ui.result_loading
        || ui.results_older_loading
        || !ui.result_pages.begin(cursor, PageDirection::Back)
    {
        return;
    }
    match cursor {
        Some(cursor) => {
            ui.results_older_loading = true;
            effects.push(Effect::LoadOlderResults {
                session,
                generation: ui.generation,
                query: ui.result_query,
                cursor,
            });
        }
        None => {
            ui.result_loading = true;
            effects.push(Effect::LoadResults {
                session,
                generation: ui.generation,
                query: ui.result_query,
            });
        }
    }
}

fn request_artifact(state: &mut UiState, result: bone_app::ResultRef, effects: &mut Vec<Effect>) {
    if state.artifact.loading {
        return;
    }
    state.artifact.query = state.generation();
    state.artifact.loading = true;
    state.artifact.artifact = None;
    state.artifact.evidence = None;
    state.artifact.source = None;
    state.artifact.source_loading = false;
    state.artifact.source_query = state.generation();
    effects.push(Effect::LoadArtifact {
        result,
        query: state.artifact.query,
    });
}

fn open_evidence_source(state: &mut UiState, index: usize, effects: &mut Vec<Effect>) {
    if state.artifact.source_loading {
        return;
    }
    let Some(artifact) = state.artifact.artifact.as_ref() else {
        return;
    };
    let Some(item) = state
        .artifact
        .evidence
        .as_ref()
        .and_then(|page| page.items.get(index))
        .cloned()
    else {
        return;
    };
    let result = artifact.result;
    state.artifact.selection = index;
    state.artifact.source_query = state.generation();
    match item.availability {
        bone_app::EvidenceAvailability::Available { .. } => {
            state.artifact.source_loading = true;
            state.artifact.source = None;
            effects.push(Effect::LoadEvidenceSource {
                result,
                query: state.artifact.source_query,
                source: item.source,
                offset: 0,
                append: false,
            });
        }
        availability => {
            state.artifact.source = Some(EvidenceReaderUi {
                result,
                source: item.source,
                availability,
                text: String::new(),
                window_offset: 0,
                next_offset: None,
                total_bytes: None,
                projection_pending: state
                    .artifact
                    .evidence
                    .as_ref()
                    .is_some_and(|page| page.projection_pending),
                frontend_truncated: false,
                layout: Default::default(),
            });
            state.detail_scroll = 0;
        }
    }
}

fn load_more_evidence_source(state: &mut UiState, effects: &mut Vec<Effect>) {
    if state.artifact.source_loading {
        return;
    }
    let Some(reader) = state.artifact.source.as_ref() else {
        return;
    };
    let Some(offset) = reader.next_offset else {
        return;
    };
    let result = reader.result;
    let source = reader.source;
    state.artifact.source_query = state.generation();
    state.artifact.source_loading = true;
    effects.push(Effect::LoadEvidenceSource {
        result,
        query: state.artifact.source_query,
        source,
        offset,
        append: true,
    });
}

fn load_previous_evidence_source(state: &mut UiState, effects: &mut Vec<Effect>) {
    if state.artifact.source_loading {
        return;
    }
    let Some(reader) = state.artifact.source.as_ref() else {
        return;
    };
    if reader.window_offset == 0 {
        return;
    }
    let result = reader.result;
    let source = reader.source;
    let offset = reader
        .window_offset
        .saturating_sub(EVIDENCE_VIEW_BYTES as u64);
    state.artifact.source_query = state.generation();
    state.artifact.source_loading = true;
    effects.push(Effect::LoadEvidenceSource {
        result,
        query: state.artifact.source_query,
        source,
        offset,
        append: false,
    });
}

fn request_previous_evidence(state: &mut UiState, effects: &mut Vec<Effect>) {
    if state.artifact.loading {
        return;
    }
    let Some(result) = state
        .artifact
        .artifact
        .as_ref()
        .map(|artifact| artifact.result)
    else {
        return;
    };
    let Some(cursor) = state.artifact.evidence_pages.back.last().copied() else {
        return;
    };
    if !state
        .artifact
        .evidence_pages
        .begin(cursor, PageDirection::Back)
    {
        return;
    }
    match cursor {
        Some(cursor) => {
            state.artifact.loading = true;
            effects.push(Effect::LoadEvidence {
                result,
                query: state.artifact.query,
                cursor,
            });
        }
        None => request_artifact(state, result, effects),
    }
}

fn request_workspace_changes(state: &mut UiState, append: bool, effects: &mut Vec<Effect>) {
    if state.workspace_changes.loading {
        return;
    }
    let Some((workspace, _)) = state.workspace.as_ref() else {
        return;
    };
    let workspace = *workspace;
    let cursor = if append {
        state
            .workspace_changes
            .page
            .as_ref()
            .and_then(|page| page.next_cursor.clone())
    } else {
        None
    };
    if append && cursor.is_none() {
        return;
    }
    let direction = if append {
        PageDirection::Forward
    } else {
        PageDirection::Refresh
    };
    if !state
        .workspace_changes
        .pages
        .begin(cursor.clone(), direction)
    {
        return;
    }
    state.workspace_changes.query = state.generation();
    state.workspace_changes.loading = true;
    if !append {
        clear_workspace_file(state);
        state.workspace_changes.file_query = state.generation();
    }
    effects.push(Effect::LoadWorkspaceChanges {
        workspace,
        query: state.workspace_changes.query,
        cursor,
        append,
    });
}

fn request_previous_workspace_changes(state: &mut UiState, effects: &mut Vec<Effect>) {
    if state.workspace_changes.loading {
        return;
    }
    let Some((workspace, _)) = state.workspace.as_ref() else {
        return;
    };
    let workspace = *workspace;
    let Some(cursor) = state.workspace_changes.pages.back.last().cloned() else {
        return;
    };
    if !state
        .workspace_changes
        .pages
        .begin(cursor.clone(), PageDirection::Back)
    {
        return;
    }
    state.workspace_changes.query = state.generation();
    state.workspace_changes.loading = true;
    clear_workspace_file(state);
    effects.push(Effect::LoadWorkspaceChanges {
        workspace,
        query: state.workspace_changes.query,
        cursor,
        append: false,
    });
}

fn open_workspace_file(state: &mut UiState, index: usize, effects: &mut Vec<Effect>) {
    if state.workspace_changes.file_loading {
        return;
    }
    let Some((workspace, _)) = state.workspace.as_ref() else {
        return;
    };
    let workspace = *workspace;
    let Some(page) = state.workspace_changes.page.as_ref() else {
        return;
    };
    let Some(file) = page.files.get(index) else {
        return;
    };
    let source = if !file.tracked
        || matches!(
            &page.baseline,
            bone_app::WorkspaceBaseline::Git { head: None }
        ) {
        bone_app::WorkspaceFileSource::WorkingTree
    } else {
        bone_app::WorkspaceFileSource::DiffAgainstHead
    };
    let path = file.path.clone();
    state.workspace_changes.selection = index;
    state.workspace_changes.file_query = state.generation();
    state.workspace_changes.file_loading = true;
    state.workspace_changes.file = None;
    state.workspace_changes.file_cursor = None;
    state.workspace_changes.file_back.clear();
    state.workspace_changes.file_request_cursor = None;
    state.workspace_changes.file_request_append = false;
    state.workspace_changes.file_request_path = Some(path.clone());
    state.workspace_changes.file_request_source = Some(source);
    state.workspace_changes.file_control_selection = 2;
    effects.push(Effect::LoadWorkspaceFile {
        workspace,
        query: state.workspace_changes.file_query,
        path,
        source,
        cursor: None,
        append: false,
    });
}

fn request_more_workspace_file(state: &mut UiState, effects: &mut Vec<Effect>) {
    let Some(cursor) = state
        .workspace_changes
        .file
        .as_ref()
        .and_then(|page| page.next_cursor.clone())
    else {
        return;
    };
    request_workspace_file_cursor(state, Some(cursor), true, effects);
}

fn request_previous_workspace_file(state: &mut UiState, effects: &mut Vec<Effect>) {
    let Some(cursor) = state.workspace_changes.file_back.last().cloned() else {
        return;
    };
    request_workspace_file_cursor(state, cursor, false, effects);
}

fn request_workspace_file_cursor(
    state: &mut UiState,
    cursor: Option<bone_app::WorkspaceFileCursor>,
    append: bool,
    effects: &mut Vec<Effect>,
) {
    if state.workspace_changes.file_loading {
        return;
    }
    let Some((workspace, _)) = state.workspace.as_ref() else {
        return;
    };
    let Some(file) = state.workspace_changes.file.as_ref() else {
        return;
    };
    let workspace = *workspace;
    let path = file.path.clone();
    let source = file.source;
    state.workspace_changes.file_query = state.generation();
    state.workspace_changes.file_loading = true;
    state.workspace_changes.file_request_cursor = cursor.clone();
    state.workspace_changes.file_request_append = append;
    state.workspace_changes.file_request_path = Some(path.clone());
    state.workspace_changes.file_request_source = Some(source);
    state.workspace_changes.file_control_selection = 2;
    effects.push(Effect::LoadWorkspaceFile {
        workspace,
        query: state.workspace_changes.file_query,
        path,
        source,
        cursor,
        append,
    });
}

fn workspace_file_control_available(state: &UiState, control: usize) -> bool {
    match control {
        0 => !state.workspace_changes.file_back.is_empty(),
        1 => state
            .workspace_changes
            .file
            .as_ref()
            .is_some_and(|page| page.next_cursor.is_some()),
        2 => state.workspace_changes.file.is_some(),
        _ => false,
    }
}

fn preferred_workspace_file_control(state: &UiState) -> usize {
    if workspace_file_control_available(state, 1) {
        1
    } else if workspace_file_control_available(state, 0) {
        0
    } else {
        2
    }
}

fn move_workspace_file_control(state: &mut UiState, delta: isize) {
    let available = (0..3)
        .filter(|control| workspace_file_control_available(state, *control))
        .collect::<Vec<_>>();
    if available.is_empty() {
        return;
    }
    let current = available
        .iter()
        .position(|control| *control == state.workspace_changes.file_control_selection)
        .unwrap_or(0);
    let next = current
        .saturating_add_signed(delta)
        .min(available.len().saturating_sub(1));
    state.workspace_changes.file_control_selection = available[next];
}

fn selected_workspace_file_action(state: &UiState) -> Action {
    match state.workspace_changes.file_control_selection {
        0 if workspace_file_control_available(state, 0) => Action::LoadPreviousWorkspaceFile,
        1 if workspace_file_control_available(state, 1) => Action::LoadMoreWorkspaceFile,
        _ => Action::CloseWorkspaceFile,
    }
}

fn clear_workspace_file(state: &mut UiState) {
    state.workspace_changes.file = None;
    state.workspace_changes.file_loading = false;
    state.workspace_changes.file_cursor = None;
    state.workspace_changes.file_back.clear();
    state.workspace_changes.file_request_cursor = None;
    state.workspace_changes.file_request_append = false;
    state.workspace_changes.file_request_path = None;
    state.workspace_changes.file_request_source = None;
    state.workspace_changes.file_control_selection = 2;
    state
        .workspace_changes
        .file_layout
        .borrow_mut()
        .lines
        .clear();
}

fn request_settings(
    state: &mut UiState,
    session: SessionId,
    generation: u64,
    effects: &mut Vec<Effect>,
) {
    state.settings_query = state.settings_query.wrapping_add(1).max(1);
    effects.push(Effect::LoadSettings {
        session,
        generation,
        query: state.settings_query,
    });
}

fn request_acceptances(
    state: &mut UiState,
    session: SessionId,
    result: bone_app::ResultRef,
    cursor: Option<bone_app::AcceptanceCursor>,
    append_older: bool,
    effects: &mut Vec<Effect>,
) {
    let direction = if append_older {
        PageDirection::Forward
    } else {
        PageDirection::Refresh
    };
    request_acceptances_page(state, session, result, cursor, direction, effects);
}

fn request_acceptances_page(
    state: &mut UiState,
    session: SessionId,
    result: bone_app::ResultRef,
    cursor: Option<bone_app::AcceptanceCursor>,
    direction: PageDirection,
    effects: &mut Vec<Effect>,
) {
    if !state.acceptance_loading.insert(result) {
        return;
    }
    if !state
        .acceptance_pages
        .entry(result)
        .or_default()
        .begin(cursor, direction)
    {
        state.acceptance_loading.remove(&result);
        return;
    }
    let Some(ui) = state.session_ui.get(&session) else {
        state.acceptance_loading.remove(&result);
        state.acceptance_pages.remove(&result);
        return;
    };
    let query = state
        .acceptance_queries
        .entry(result)
        .and_modify(|value| *value = value.wrapping_add(1).max(1))
        .or_insert(1);
    effects.push(Effect::LoadAcceptances {
        session,
        generation: ui.generation,
        result,
        query: *query,
        cursor,
        append_older: direction == PageDirection::Forward,
    });
}

fn request_previous_acceptances(state: &mut UiState, effects: &mut Vec<Effect>) {
    let Some(session) = state.selected else {
        return;
    };
    let Some(result) = state
        .results
        .get(&session)
        .and_then(|page| page.items.last())
        .map(|item| item.result)
    else {
        return;
    };
    let Some(cursor) = state
        .acceptance_pages
        .get(&result)
        .and_then(|pages| pages.back.last().copied())
    else {
        return;
    };
    request_acceptances_page(state, session, result, cursor, PageDirection::Back, effects);
}

fn move_selection(state: &mut UiState, delta: isize, effects: &mut Vec<Effect>) {
    if state.sessions.is_empty() {
        return;
    }
    let current = state
        .selected
        .and_then(|id| state.sessions.iter().position(|item| item.id == id))
        .unwrap_or(0);
    let next = current
        .saturating_add_signed(delta)
        .min(state.sessions.len().saturating_sub(1));
    let id = state.sessions[next].id;
    let remain_in_browser = state.main == MainView::Sessions;
    select_session(state, id, effects);
    if remain_in_browser {
        state.main = MainView::Sessions;
        state.focus = Focus::Timeline;
    }
}

fn edit_selected(
    state: &mut UiState,
    edit: impl FnOnce(&mut SessionUi),
    _effects: &mut Vec<Effect>,
) {
    if state.main != MainView::Workbench || state.focus != Focus::Composer {
        return;
    }
    if let Some(ui) = state.selected_ui_mut() {
        edit(ui);
        ui.draft_revision = ui.draft_revision.wrapping_add(1);
        if let Some(pending) = &mut ui.submitting {
            pending.clear_on_receipt &= ui.draft.starts_with(&pending.text);
        }
    }
}

fn submit_selected(state: &mut UiState, effects: &mut Vec<Effect>) {
    if state.main != MainView::Workbench {
        return;
    }
    if state.selected_ui().is_some_and(|ui| ui.snapshot.is_none()) {
        state.status = Some("会话正在打开，草稿已经保留，请稍后发送".into());
        return;
    }
    if let Some(ui) = state.selected_ui_mut() {
        if let Some(pending) = &mut ui.submitting {
            if pending.failed {
                pending.failed = false;
                let mut input = SubmitInput::new(pending.text.clone());
                input.request_id = pending.request_id;
                input.reply_to = pending.reply_to;
                effects.push(Effect::Submit {
                    session: ui.info.id,
                    generation: ui.generation,
                    input,
                });
            }
            return;
        }
        if ui.draft.trim().is_empty() {
            return;
        }
        let mut input = SubmitInput::new(ui.draft.clone());
        input.reply_to = ui.reply_to;
        let pending = PendingSubmission {
            request_id: input.request_id,
            text: input.text.clone(),
            draft_revision: ui.draft_revision,
            reply_to: input.reply_to,
            failed: false,
            clear_on_receipt: true,
        };
        ui.submitting = Some(pending);
        effects.push(Effect::Submit {
            session: ui.info.id,
            generation: ui.generation,
            input,
        });
    }
}

fn remove_last_grapheme(value: &mut String) {
    if let Some((index, _)) = value.grapheme_indices(true).next_back() {
        value.truncate(index);
    }
}

fn enforce_history_budget(state: &mut UiState) {
    for ui in state.session_ui.values_mut() {
        while ui.history.len() > HISTORY_CACHE_ITEMS {
            let item = if ui.scroll_from_tail > 0 {
                ui.newer_history_missing = true;
                ui.scroll_from_tail = ui.scroll_from_tail.saturating_sub(1);
                ui.history.pop_back()
            } else {
                ui.history_window_stale = true;
                ui.history.pop_front()
            };
            let Some(item) = item else {
                break;
            };
            ui.history_bytes = ui.history_bytes.saturating_sub(history_entry_bytes(&item));
        }
    }
    let detail_bytes = enforce_detail_budget(state);
    let history_budget = HISTORY_CACHE_BYTES.saturating_sub(detail_bytes);
    let mut total = state
        .session_ui
        .values()
        .map(|ui| ui.history_bytes)
        .sum::<usize>();
    if total <= history_budget {
        return;
    }
    let selected = state.selected;
    for (id, ui) in &mut state.session_ui {
        if Some(*id) == selected {
            continue;
        }
        total = total.saturating_sub(ui.history_bytes);
        ui.history.clear();
        ui.history_bytes = 0;
        ui.snapshot = None;
        ui.history_window_stale = false;
        ui.newer_history_missing = false;
        if total <= history_budget {
            return;
        }
    }
    if let Some(ui) = state.selected_ui_mut() {
        while total > history_budget {
            let item = if ui.scroll_from_tail > 0 {
                ui.newer_history_missing = true;
                ui.scroll_from_tail = ui.scroll_from_tail.saturating_sub(1);
                ui.history.pop_back()
            } else {
                ui.history_window_stale = true;
                ui.history.pop_front()
            };
            let Some(item) = item else {
                break;
            };
            let bytes = history_entry_bytes(&item);
            ui.history_bytes = ui.history_bytes.saturating_sub(bytes);
            total = total.saturating_sub(bytes);
        }
    }
}

fn enforce_detail_budget(state: &mut UiState) -> usize {
    let selected = state.selected;
    let current_result = selected
        .and_then(|session| state.results.get(&session))
        .and_then(|page| page.items.last())
        .map(|summary| summary.result);
    let mut total = detail_cache_bytes(state);

    let stale_acceptances = state
        .acceptances
        .keys()
        .copied()
        .filter(|result| Some(*result) != current_result)
        .collect::<Vec<_>>();
    for result in stale_acceptances {
        if total <= HISTORY_CACHE_BYTES {
            break;
        }
        if let Some(page) = state.acceptances.remove(&result) {
            total = total.saturating_sub(acceptance_page_bytes(&page));
            state.acceptance_queries.remove(&result);
            state.acceptance_loading.remove(&result);
            state.acceptance_pages.remove(&result);
        }
    }

    let stale_results = state
        .results
        .keys()
        .copied()
        .filter(|session| Some(*session) != selected)
        .collect::<Vec<_>>();
    for session in stale_results {
        if total <= HISTORY_CACHE_BYTES {
            break;
        }
        if let Some(page) = state.results.remove(&session) {
            total = total.saturating_sub(result_page_bytes(&page));
        }
    }

    if total > HISTORY_CACHE_BYTES {
        state
            .workspace_changes
            .file_layout
            .borrow_mut()
            .lines
            .clear();
        state
            .workspace_changes
            .file_layout
            .borrow_mut()
            .lines
            .shrink_to_fit();
        if let Some(source) = state.artifact.source.as_ref() {
            source.layout.borrow_mut().lines.clear();
            source.layout.borrow_mut().lines.shrink_to_fit();
        }
    }
    detail_cache_bytes(state)
}

fn detail_cache_bytes(state: &UiState) -> usize {
    let results = state.results.values().map(result_page_bytes).sum::<usize>();
    let acceptances = state
        .acceptances
        .values()
        .map(acceptance_page_bytes)
        .sum::<usize>();
    let changes = state.workspace_changes.page.as_ref().map_or(0, |page| {
        page.files
            .iter()
            .map(|file| std::mem::size_of::<bone_app::WorkspaceChangedFile>() + file.path.len())
            .sum()
    });
    let file = state.workspace_changes.file.as_ref().map_or(0, |file| {
        std::mem::size_of::<bone_app::WorkspaceFilePage>()
            + file.path.len()
            + file.text.as_ref().map_or(0, String::len)
    });
    let file_layout = state
        .workspace_changes
        .file_layout
        .borrow()
        .lines
        .capacity()
        * std::mem::size_of::<(usize, usize)>();
    let artifact = state.artifact.artifact.as_ref().map_or(0, |artifact| {
        std::mem::size_of::<bone_app::ResultArtifact>()
            + artifact.summary.len()
            + artifact.remaining.iter().map(String::len).sum::<usize>()
    });
    let evidence = state.artifact.evidence.as_ref().map_or(0, |page| {
        page.items.iter().map(evidence_summary_bytes).sum()
    });
    let source = state.artifact.source.as_ref().map_or(0, |source| {
        source.text.len()
            + source.layout.borrow().lines.capacity() * std::mem::size_of::<(usize, usize)>()
    });
    let navigation = state
        .session_ui
        .values()
        .map(|ui| page_navigation_bytes(&ui.result_pages))
        .sum::<usize>()
        .saturating_add(
            state
                .acceptance_pages
                .values()
                .map(page_navigation_bytes)
                .sum(),
        )
        .saturating_add(page_navigation_bytes(&state.workspace_changes.pages))
        .saturating_add(page_navigation_bytes(&state.artifact.evidence_pages));
    results
        .saturating_add(acceptances)
        .saturating_add(changes)
        .saturating_add(file)
        .saturating_add(file_layout)
        .saturating_add(artifact)
        .saturating_add(evidence)
        .saturating_add(source)
        .saturating_add(navigation)
}

fn page_navigation_bytes<C>(pages: &PageNavigation<C>) -> usize {
    pages
        .back
        .capacity()
        .saturating_mul(std::mem::size_of::<Option<C>>())
        .saturating_add(std::mem::size_of::<PageNavigation<C>>())
}

fn result_page_bytes(page: &bone_app::ResultPage) -> usize {
    page.items.iter().map(result_summary_bytes).sum()
}

fn result_summary_bytes(summary: &bone_app::ResultSummary) -> usize {
    std::mem::size_of::<bone_app::ResultSummary>()
        + summary.summary.len()
        + summary.remaining.iter().map(String::len).sum::<usize>()
}

fn acceptance_page_bytes(page: &bone_app::AcceptancePage) -> usize {
    page.items.iter().map(acceptance_record_bytes).sum()
}

fn acceptance_record_bytes(record: &bone_app::AcceptanceRecord) -> usize {
    std::mem::size_of::<bone_app::AcceptanceRecord>() + record.reason.len()
}

fn evidence_summary_bytes(summary: &bone_app::EvidenceSummary) -> usize {
    std::mem::size_of::<bone_app::EvidenceSummary>()
        + match &summary.availability {
            bone_app::EvidenceAvailability::Available { title, .. } => title.len(),
            bone_app::EvidenceAvailability::Private | bone_app::EvidenceAvailability::Missing => 0,
        }
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

fn session_management_response_is_current(
    state: &UiState,
    session: SessionId,
    generation: u64,
    operation: u64,
) -> bool {
    state
        .session_ui
        .get(&session)
        .is_some_and(|ui| ui.generation == generation)
        && state.session_management_operations.get(&session) == Some(&operation)
}

fn update_session_info(state: &mut UiState, session: SessionId, update: impl Fn(&mut SessionInfo)) {
    if let Some(info) = state.sessions.iter_mut().find(|info| info.id == session) {
        update(info);
    }
    if let Some(ui) = state.session_ui.get_mut(&session) {
        update(&mut ui.info);
    }
}

fn queue_history_until(
    ui: &mut SessionUi,
    history_through: bone_app::SessionSeq,
    effects: &mut Vec<Effect>,
) {
    if !ui.history_loading && ui.history_cursor < history_through {
        ui.history_loading = true;
        effects.push(Effect::LoadHistory {
            session: ui.info.id,
            generation: ui.generation,
            after: ui.history_cursor,
        });
    }
}

fn next_focus(focus: Focus, reverse: bool) -> Focus {
    let values = [
        Focus::Global,
        Focus::Rail,
        Focus::Timeline,
        Focus::Composer,
        Focus::Detail,
        Focus::Actions,
    ];
    let index = values.iter().position(|item| *item == focus).unwrap_or(0);
    let next = if reverse {
        index.checked_sub(1).unwrap_or(values.len() - 1)
    } else {
        (index + 1) % values.len()
    };
    values[next]
}

const MAIN_VIEWS: [MainView; 4] = [
    MainView::Workbench,
    MainView::Sessions,
    MainView::Attention,
    MainView::Settings,
];

const DETAIL_KINDS: [DetailKind; 6] = [
    DetailKind::Work,
    DetailKind::Changes,
    DetailKind::Context,
    DetailKind::Artifacts,
    DetailKind::Records,
    DetailKind::Acceptance,
];

fn main_view_at(index: usize) -> MainView {
    MAIN_VIEWS[index.min(MAIN_VIEWS.len() - 1)]
}

fn main_view_index(view: MainView) -> usize {
    MAIN_VIEWS
        .iter()
        .position(|item| *item == view)
        .unwrap_or(0)
}

fn detail_kind_at(index: usize) -> DetailKind {
    DETAIL_KINDS[index.min(DETAIL_KINDS.len() - 1)]
}

fn detail_kind_index(kind: DetailKind) -> usize {
    DETAIL_KINDS
        .iter()
        .position(|item| *item == kind)
        .unwrap_or(0)
}

fn detail_state_at(index: usize) -> DetailState {
    let kind = detail_kind_at(index);
    let title = match kind {
        DetailKind::Work => "工作详情",
        DetailKind::Changes => "工作区变更",
        DetailKind::Context => "会话上下文",
        DetailKind::Artifacts => "产物与证据",
        DetailKind::Records => "事实记录",
        DetailKind::Acceptance => "结果与验收",
        DetailKind::Decision => unreachable!("Decision is not a navigation tab"),
    };
    DetailState {
        kind,
        title: title.into(),
    }
}

fn move_focused_control(state: &mut UiState, delta: isize) {
    let (selection, length) = match state.focus {
        Focus::Global => (&mut state.global_selection, MAIN_VIEWS.len()),
        Focus::Detail
            if state
                .detail
                .as_ref()
                .is_some_and(|detail| detail.kind != DetailKind::Decision) =>
        {
            (&mut state.detail_tab_selection, DETAIL_KINDS.len())
        }
        Focus::Actions => (
            &mut state.action_selection,
            if state.main == MainView::Workbench {
                4
            } else {
                1
            },
        ),
        _ => return,
    };
    *selection = selection
        .saturating_add_signed(delta)
        .min(length.saturating_sub(1));
}

fn selected_action_bar_action(state: &UiState) -> Action {
    if state.main != MainView::Workbench {
        return Action::Quit;
    }
    match state.action_selection.min(3) {
        0 => Action::OpenDetail(detail_state_at(0)),
        1 => Action::SubmitFromActionBar,
        2 => Action::Stop,
        _ => Action::Quit,
    }
}
