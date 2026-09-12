use std::sync::Arc;

use crate::{
    layout::{HitTarget, SinglePane},
    state::{
        Action, CursorMove, EditCommand, EditorTarget, Effect, Focus, OperationKind, SessionStatus,
        SessionUi, UiEvent, UiState, update,
    },
    view,
};
use bone_app::{
    HistoryEntry, HistoryPage, RecentHistoryPage, RequestId, SessionEvent, SessionId, SessionInfo,
    SessionReleaseReceipt, SessionReleaseStatus, SessionView, WorkspaceId,
};
use ratatui::{Terminal, backend::TestBackend, layout::Rect};

fn info(workspace: WorkspaceId, title: &str) -> SessionInfo {
    SessionInfo {
        id: SessionId::new(),
        workspace,
        title: title.into(),
        archived: false,
    }
}

fn snapshot(info: &SessionInfo, through: u64) -> Arc<SessionView> {
    Arc::new(SessionView {
        session: info.clone(),
        runtime: bone_app::RuntimeState::Detached,
        draft: String::new(),
        inputs: Vec::new(),
        jobs: Vec::new(),
        activity: Vec::new(),
        history_through: bone_app::SessionSeq(through),
        problem: None,
    })
}

fn opened(infos: &[SessionInfo]) -> UiState {
    let mut state = UiState::default();
    state.workspace = infos
        .first()
        .map(|item| (item.workspace, "regression workspace".into()));
    state.sessions = infos.to_vec();
    state.selected = infos.first().map(|item| item.id);
    for item in infos {
        let mut ui = SessionUi::new(item.clone(), 1);
        ui.snapshot = Some(snapshot(item, 0));
        ui.hydrated = true;
        state.session_ui.insert(item.id, ui);
    }
    state
}

fn paste(state: &mut UiState, text: &str) {
    update(
        state,
        UiEvent::Action(editor(EditCommand::Insert {
            text: text.into(),
            typing: false,
        })),
    );
}

fn editor(command: EditCommand) -> Action {
    Action::Edit {
        target: EditorTarget::Composer,
        command,
    }
}

fn create_effect(effects: &[Effect]) -> (RequestId, String, bool) {
    effects
        .iter()
        .find_map(|effect| match effect {
            Effect::CreateSession {
                request_id,
                title,
                provisional,
            } => Some((*request_id, title.clone(), *provisional)),
            _ => None,
        })
        .expect("create-session effect")
}

#[test]
fn stale_submit_failure_identity_cannot_mark_another_sessions_request_failed() {
    let workspace = WorkspaceId::new();
    let a = info(workspace, "A");
    let b = info(workspace, "B");
    let mut state = opened(&[a.clone(), b.clone()]);

    paste(&mut state, "request A");
    let a_effects = update(&mut state, UiEvent::Action(Action::Submit));
    let a_request = a_effects
        .iter()
        .find_map(|effect| match effect {
            Effect::Submit { input, .. } => Some(input.request_id),
            _ => None,
        })
        .unwrap();
    update(&mut state, UiEvent::Action(Action::SelectSession(b.id)));
    update(&mut state, UiEvent::Action(Action::Focus(Focus::Composer)));
    paste(&mut state, "request B");
    let b_effects = update(&mut state, UiEvent::Action(Action::Submit));
    let b_request = b_effects
        .iter()
        .find_map(|effect| match effect {
            Effect::Submit { input, .. } => Some(input.request_id),
            _ => None,
        })
        .unwrap_or_else(|| {
            panic!(
                "missing B submit; effects={b_effects:?}, focus={:?}, selected={:?}, draft={:?}",
                state.focus,
                state.selected,
                state.draft()
            )
        });

    update(
        &mut state,
        UiEvent::SubmitFailed {
            session: b.id,
            request_id: a_request,
            message: "stale A failure".into(),
        },
    );
    assert_eq!(
        state.session_ui[&b.id]
            .submitting
            .as_ref()
            .unwrap()
            .request_id,
        b_request
    );
    assert!(!state.session_ui[&b.id].submitting.as_ref().unwrap().failed);
    assert!(state.status.as_deref() != Some("stale A failure"));
}

#[test]
fn release_receipt_for_a_reselected_session_forces_a_fresh_open_generation() {
    let workspace = WorkspaceId::new();
    let a = info(workspace, "A");
    let b = info(workspace, "B");
    let mut state = opened(&[a.clone(), b.clone()]);
    let released_generation = state.session_ui[&a.id].generation;

    let to_b = update(&mut state, UiEvent::Action(Action::SelectSession(b.id)));
    assert!(to_b.iter().any(|effect| matches!(effect, Effect::ReleaseSession { session, generation } if *session == a.id && *generation == released_generation)));
    update(&mut state, UiEvent::Action(Action::SelectSession(a.id)));
    let generation_before_receipt = state.session_ui[&a.id].generation;

    let effects = update(
        &mut state,
        UiEvent::SessionReleased {
            generation: released_generation,
            receipt: SessionReleaseReceipt {
                session: a.id,
                status: SessionReleaseStatus::Released,
            },
        },
    );
    let reopened_generation = effects
        .iter()
        .find_map(|effect| match effect {
            Effect::OpenSession {
                session,
                generation,
            } if *session == a.id => Some(*generation),
            _ => None,
        })
        .expect("selected session is reopened after its delayed release completes");
    assert!(reopened_generation > generation_before_receipt);
    assert_eq!(state.session_ui[&a.id].generation, reopened_generation);
    assert!(state.session_ui[&a.id].snapshot.is_none());
    assert!(!state.session_ui[&a.id].hydrated);
}

#[test]
fn new_from_existing_session_clears_only_the_unchanged_source_draft() {
    let workspace = WorkspaceId::new();
    let source = info(workspace, "source");
    let mut unchanged = opened(std::slice::from_ref(&source));
    paste(&mut unchanged, "/new named");
    let created = update(&mut unchanged, UiEvent::Action(Action::Submit));
    let (request_id, _, _) = create_effect(&created);
    update(
        &mut unchanged,
        UiEvent::SessionCreated {
            request_id,
            info: info(workspace, "named"),
        },
    );
    assert!(unchanged.session_ui[&source.id].draft().is_empty());

    let mut edited = opened(std::slice::from_ref(&source));
    paste(&mut edited, "/new named");
    let created = update(&mut edited, UiEvent::Action(Action::Submit));
    let (request_id, _, _) = create_effect(&created);
    paste(&mut edited, " plus a newer edit");
    update(
        &mut edited,
        UiEvent::SessionCreated {
            request_id,
            info: info(workspace, "named"),
        },
    );
    assert_eq!(
        edited.session_ui[&source.id].draft(),
        "/new named plus a newer edit"
    );
}

#[test]
fn create_failure_retry_reuses_the_exact_request_contract() {
    let mut state = UiState::default();
    paste(&mut state, "/new stable title");
    let first = update(&mut state, UiEvent::Action(Action::Submit));
    let contract = create_effect(&first);
    update(
        &mut state,
        UiEvent::SessionCreateFailed {
            request_id: contract.0,
            message: "temporary failure".into(),
        },
    );
    let retry = update(&mut state, UiEvent::Action(Action::Submit));
    assert_eq!(create_effect(&retry), contract);
}

#[test]
fn edited_failed_new_does_not_replay_and_explicit_retry_reuses_identity() {
    let mut state = UiState::default();
    paste(&mut state, "/new A");
    let first = update(&mut state, UiEvent::Action(Action::Submit));
    let contract_a = create_effect(&first);
    assert_eq!(contract_a.1, "A");
    assert!(!contract_a.2);
    update(
        &mut state,
        UiEvent::SessionCreateFailed {
            request_id: contract_a.0,
            message: "creation A failed".into(),
        },
    );

    replace_orphan_draft(&mut state, "/new B");
    let changed = update(&mut state, UiEvent::Action(Action::Submit));
    assert!(
        !changed
            .iter()
            .any(|effect| matches!(effect, Effect::CreateSession { .. })),
        "editing the failed command must not silently replay A: {changed:?}"
    );
    let pending = state
        .pending_create
        .as_ref()
        .expect("failed A remains explicit");
    assert_eq!(pending.request_id, contract_a.0);
    assert_eq!(pending.title, "A");
    assert!(pending.failed);
    assert!(
        state
            .status
            .as_deref()
            .is_some_and(|message| message.contains("--retry"))
    );

    replace_orphan_draft(&mut state, "/new --retry");
    let retried = update(&mut state, UiEvent::Action(Action::Submit));
    assert_eq!(
        create_effect(&retried),
        contract_a,
        "explicit retry must reuse A's complete idempotency contract"
    );
}

#[test]
fn escape_dismisses_slash_palette_without_destroying_its_draft() {
    for draft in ["/n", "/does-not-exist"] {
        let mut state = UiState::default();
        paste(&mut state, draft);
        assert!(state.slash_palette_visible());
        let identity = state.draft_identity();
        assert!(update(&mut state, UiEvent::Action(Action::Escape)).is_empty());
        assert_eq!(state.draft(), draft);
        assert_eq!(state.slash_dismissed, Some(identity));
        assert!(!state.slash_palette_visible());
        assert!(state.slash_matches().is_empty());
        assert_eq!(state.focus, Focus::Composer);
    }
}

#[test]
fn a_slash_draft_does_not_capture_escape_after_focus_leaves_the_composer() {
    let mut state = UiState::default();
    paste(&mut state, "/");
    assert!(state.slash_palette_visible());
    update(&mut state, UiEvent::Action(Action::FocusLeft));
    assert_eq!(state.focus, Focus::Sessions);
    assert!(!state.slash_palette_visible());

    update(&mut state, UiEvent::Action(Action::Escape));

    assert_eq!(state.draft(), "/");
    assert_eq!(state.slash_dismissed, None);
}

#[test]
fn slash_quit_clears_command_text_while_quit_action_preserves_normal_draft() {
    let workspace = WorkspaceId::new();
    let session = info(workspace, "quit");
    let mut slash = opened(std::slice::from_ref(&session));
    paste(&mut slash, "/quit");
    let effects = update(&mut slash, UiEvent::Action(Action::Submit));
    assert!(slash.session_ui[&session.id].draft().is_empty());
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::SaveDraft { text, .. } if text.is_empty()))
    );
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::Shutdown))
    );

    let mut shortcut = opened(std::slice::from_ref(&session));
    paste(&mut shortcut, "unsent normal draft");
    let effects = update(&mut shortcut, UiEvent::Action(Action::Quit));
    assert_eq!(
        shortcut.session_ui[&session.id].draft(),
        "unsent normal draft"
    );
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::Shutdown))
    );
}

#[test]
fn live_history_load_is_deduplicated_and_cursor_never_moves_backwards() {
    let workspace = WorkspaceId::new();
    let session = info(workspace, "history");
    let mut state = opened(std::slice::from_ref(&session));
    let generation = state.session_ui[&session.id].generation;

    let first = update(
        &mut state,
        UiEvent::SessionChanged {
            session: session.id,
            generation,
            snapshot: snapshot(&session, 10),
        },
    );
    assert!(
        matches!(first.as_slice(), [Effect::LoadHistory { after, .. }] if *after == bone_app::SessionSeq(0))
    );
    let duplicate = update(
        &mut state,
        UiEvent::SessionChanged {
            session: session.id,
            generation,
            snapshot: snapshot(&session, 10),
        },
    );
    assert!(duplicate.is_empty());

    update(
        &mut state,
        UiEvent::HistoryLoaded {
            session: session.id,
            generation,
            page: HistoryPage {
                items: vec![cancelled(5)],
                next_cursor: bone_app::SessionSeq(5),
                has_more: false,
            },
        },
    );
    update(
        &mut state,
        UiEvent::HistoryLoaded {
            session: session.id,
            generation,
            page: HistoryPage {
                items: vec![cancelled(5), cancelled(4)],
                next_cursor: bone_app::SessionSeq(3),
                has_more: false,
            },
        },
    );
    let ui = &state.session_ui[&session.id];
    assert_eq!(ui.history_cursor, bone_app::SessionSeq(5));
    assert_eq!(ui.history.len(), 1);
}

#[test]
fn older_history_stays_capped_and_failure_releases_the_loading_latch() {
    let workspace = WorkspaceId::new();
    let session = info(workspace, "older");
    let mut state = opened(std::slice::from_ref(&session));
    let generation = state.session_ui[&session.id].generation;
    state.session_ui.get_mut(&session.id).unwrap().older_loading = true;

    update(
        &mut state,
        UiEvent::OlderHistoryLoaded {
            session: session.id,
            generation,
            page: RecentHistoryPage {
                items: (1..=700).map(cancelled).collect(),
                older_cursor: None,
                snapshot_through: bone_app::SessionSeq(700),
            },
        },
    );
    let ui = &state.session_ui[&session.id];
    assert_eq!(ui.history.len(), crate::state::HISTORY_CACHE_ITEMS);
    assert!(ui.newer_history_missing);

    state.session_ui.get_mut(&session.id).unwrap().older_loading = true;
    update(
        &mut state,
        UiEvent::OperationFailed {
            kind: OperationKind::LoadOlderHistory,
            session: Some(session.id),
            generation: Some(generation),
            message: "retryable".into(),
        },
    );
    assert!(!state.session_ui[&session.id].older_loading);
}

#[test]
fn total_history_bytes_are_bounded_across_sessions_with_large_replies() {
    let workspace = WorkspaceId::new();
    let sessions = [
        info(workspace, "A"),
        info(workspace, "B"),
        info(workspace, "C"),
    ];
    let mut state = opened(&sessions);
    for session in &sessions {
        let generation = state.session_ui[&session.id].generation;
        let items = (1..=12)
            .map(|sequence| large_reply(sequence, 1024 * 1024))
            .collect();
        update(
            &mut state,
            UiEvent::HistoryLoaded {
                session: session.id,
                generation,
                page: HistoryPage {
                    items,
                    next_cursor: bone_app::SessionSeq(12),
                    has_more: false,
                },
            },
        );
    }
    let total: usize = state.session_ui.values().map(|ui| ui.history_bytes).sum();
    assert!(
        total <= crate::state::HISTORY_CACHE_BYTES,
        "retained {total} bytes"
    );
}

#[test]
fn rail_viewport_keeps_selection_visible_and_footer_hits_the_rail() {
    let plan = crate::layout::LayoutPlan::calculate_with_widths(
        Rect::new(0, 0, 120, 14),
        SinglePane::Conversation,
        20,
        Some(17),
        None,
        1,
        crate::layout::PaneWidths::default(),
    );
    assert!(plan.session_start > 0);
    assert!(plan.session_rows.iter().any(|row| row.index == 17));
    let selected = plan
        .session_rows
        .iter()
        .find(|row| row.index == 17)
        .unwrap();
    let rail = plan.session_rail.unwrap();
    assert!(selected.area.x >= rail.x && selected.area.right() <= rail.right());

    let workspace = WorkspaceId::new();
    let sessions: Vec<_> = (0..20)
        .map(|index| info(workspace, &format!("S{index}")))
        .collect();
    let target = sessions[17].id;
    let mut state = opened(&sessions);
    state.focus = Focus::Sessions;
    update(&mut state, UiEvent::Action(Action::SelectSession(target)));
    assert_eq!(state.selected, Some(target));
    assert_eq!(state.session_candidate, Some(target));
    assert_eq!(state.focus, Focus::Sessions);
    let rendered = render_plan(&state, 120, 14);
    let rail = rendered.layout.session_rail.unwrap();
    assert_eq!(
        rendered.hit(rail.x + 1, rail.bottom() - 1),
        Some(HitTarget::SessionRail)
    );
}

#[test]
fn session_rail_renders_draft_attention_and_recoverable_statuses() {
    let workspace = WorkspaceId::new();
    let draft = info(workspace, "draft session");
    let attention = info(workspace, "attention session");
    let recoverable = info(workspace, "recoverable session");
    let mut state = UiState::default();
    state.workspace = Some((workspace, "status workspace".into()));
    state.sessions = vec![draft.clone(), attention.clone(), recoverable.clone()];
    state
        .session_statuses
        .insert(draft.id, SessionStatus::Draft);
    state
        .session_statuses
        .insert(attention.id, SessionStatus::NeedsAttention);
    state
        .session_statuses
        .insert(recoverable.id, SessionStatus::Recoverable);

    let backend = TestBackend::new(120, 20);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|frame| {
            view::render(frame, &state);
        })
        .expect("render session statuses");
    let buffer = terminal.backend().buffer();
    let screen: String = buffer.content().iter().map(|cell| cell.symbol()).collect();
    for expected in ["draft session", "attention session", "recoverable session"] {
        assert!(
            screen.contains(expected),
            "missing {expected:?} in {screen:?}"
        );
    }
    assert_eq!(
        buffer
            .content()
            .iter()
            .filter(|cell| cell.symbol() == "•")
            .count(),
        3,
        "each semantic Session status should remain visible without replacing the fixed three-line content"
    );
    for obsolete_label in ["Needs you", "Can resume"] {
        assert!(!screen.contains(obsolete_label));
    }
}

#[test]
fn grapheme_editor_supports_middle_insert_delete_and_per_line_home_end() {
    let mut state = UiState::default();
    paste(&mut state, "A👨‍👩‍👧‍👦B\n中文");
    update(
        &mut state,
        UiEvent::Action(editor(EditCommand::Move {
            cursor: CursorMove::LineStart,
            select: false,
        })),
    );
    assert_eq!(state.draft_cursor(), "A👨‍👩‍👧‍👦B\n".len());
    update(
        &mut state,
        UiEvent::Action(editor(EditCommand::Move {
            cursor: CursorMove::Left,
            select: false,
        })),
    );
    update(
        &mut state,
        UiEvent::Action(editor(EditCommand::Insert {
            text: "!".into(),
            typing: true,
        })),
    );
    assert_eq!(state.draft(), "A👨‍👩‍👧‍👦B!\n中文");
    update(
        &mut state,
        UiEvent::Action(editor(EditCommand::DeleteBefore)),
    );
    assert_eq!(state.draft(), "A👨‍👩‍👧‍👦B\n中文");
    for _ in 0..2 {
        update(
            &mut state,
            UiEvent::Action(editor(EditCommand::Move {
                cursor: CursorMove::Left,
                select: false,
            })),
        );
    }
    update(
        &mut state,
        UiEvent::Action(editor(EditCommand::DeleteAfter)),
    );
    assert_eq!(state.draft(), "AB\n中文");
    update(
        &mut state,
        UiEvent::Action(editor(EditCommand::Move {
            cursor: CursorMove::LineEnd,
            select: false,
        })),
    );
    assert_eq!(state.draft_cursor(), "AB".len());
}

#[test]
fn visual_row_metrics_prevent_early_older_history_prefetch() {
    let workspace = WorkspaceId::new();
    let session = info(workspace, "long reply");
    let mut state = opened(std::slice::from_ref(&session));
    let ui = state.session_ui.get_mut(&session.id).unwrap();
    ui.history.push_back(large_reply(1, 20_000));
    let metrics = render_plan(&state, 120, 30)
        .transcript_metrics
        .expect("rendered transcript metrics");
    assert!(metrics.total_rows > metrics.viewport_rows + 10);

    let effects = update(
        &mut state,
        UiEvent::Action(Action::ScrollUp {
            amount: 1,
            metrics: Some(metrics),
        }),
    );
    assert_eq!(state.session_ui[&session.id].scroll_from_tail, 1);
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, Effect::LoadOlderHistory { .. })),
        "one visual row from the tail is nowhere near the start of a long reply"
    );
}

#[test]
fn evicting_newer_messages_compensates_the_visual_scroll_anchor() {
    let workspace = WorkspaceId::new();
    let session = info(workspace, "anchored history");
    let mut state = opened(std::slice::from_ref(&session));
    let ui = state.session_ui.get_mut(&session.id).unwrap();
    ui.history = (1_000..1_512)
        .map(|sequence| large_reply(sequence, 160))
        .collect();
    ui.history_bytes = ui.history.iter().map(history_bytes).sum();

    let metrics = render_plan(&state, 100, 24)
        .transcript_metrics
        .expect("rendered transcript metrics");
    let evicted_rows = (1_480..1_512)
        .map(|sequence| metrics.anchors.row_count(bone_app::SessionSeq(sequence)))
        .sum::<usize>();
    assert!(evicted_rows > 32, "wrapped messages occupy multiple rows");
    let ui = state.session_ui.get_mut(&session.id).unwrap();
    ui.scroll_from_tail = evicted_rows + 7;
    ui.older_loading = true;
    ui.older_metrics = Some(metrics);
    let generation = ui.generation;

    update(
        &mut state,
        UiEvent::OlderHistoryLoaded {
            session: session.id,
            generation,
            page: RecentHistoryPage {
                items: (1..=32).map(cancelled).collect(),
                older_cursor: None,
                snapshot_through: bone_app::SessionSeq(1_511),
            },
        },
    );
    assert_eq!(state.session_ui[&session.id].scroll_from_tail, 7);
    assert!(state.session_ui[&session.id].newer_history_missing);
}

fn render_plan(state: &UiState, width: u16, height: u16) -> view::FrameSnapshot {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut plan = None;
    terminal
        .draw(|frame| plan = Some(view::render(frame, state)))
        .unwrap();
    plan.unwrap()
}

fn cancelled(sequence: u64) -> HistoryEntry {
    HistoryEntry {
        sequence: bone_app::SessionSeq(sequence),
        occurred_at: sequence as i64,
        event: SessionEvent::InputCancelled {
            input: bone_app::InputId(sequence),
        },
    }
}

fn large_reply(sequence: u64, bytes: usize) -> HistoryEntry {
    HistoryEntry {
        sequence: bone_app::SessionSeq(sequence),
        occurred_at: sequence as i64,
        event: SessionEvent::Reply {
            job: bone_app::JobRef {
                runtime: bone_app::RuntimeId::new(),
                id: sequence,
            },
            inputs: Vec::new(),
            text: "x".repeat(bytes),
        },
    }
}

fn history_bytes(entry: &HistoryEntry) -> usize {
    serde_json::to_vec(entry).unwrap().len()
}

fn replace_orphan_draft(state: &mut UiState, text: &str) {
    update(state, UiEvent::Action(editor(EditCommand::Clear)));
    paste(state, text);
    state.slash_dismissed = None;
}
