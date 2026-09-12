use std::sync::Arc;

use crate::{
    layout::{HitTarget, SinglePane},
    state::{
        Action, EditCommand, EditorTarget, Effect, Focus, SessionNavRow, SessionUi, UiEvent,
        UiState, update,
    },
    view,
};
use bone_app::{
    HistoryEntry, RecentHistoryPage, RequestId, SessionEvent, SessionId, SessionInfo, SessionView,
    WorkspaceId,
};
use ratatui::{Terminal, backend::TestBackend, layout::Rect};

fn session(workspace: WorkspaceId, title: &str) -> SessionInfo {
    SessionInfo {
        id: SessionId::new(),
        workspace,
        title: title.into(),
        archived: false,
    }
}

fn opened_state(infos: &[SessionInfo]) -> UiState {
    let mut state = UiState::default();
    state.workspace_label = infos.first().map(|_| "contract workspace".into());
    state.session_rows = infos
        .iter()
        .cloned()
        .map(SessionNavRow::provisional)
        .collect();
    state.selected = infos.first().map(|info| info.id);
    for info in infos {
        let mut ui = SessionUi::new(info.id, 1);
        ui.snapshot = Some(snapshot(info, ""));
        ui.hydrated = true;
        state.session_ui.insert(info.id, ui);
    }
    state
}

fn snapshot(info: &SessionInfo, draft: &str) -> Arc<SessionView> {
    Arc::new(SessionView {
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

fn render(state: &UiState, width: u16, height: u16) -> (String, view::FrameSnapshot) {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    let mut plan = None;
    terminal
        .draw(|frame| plan = Some(view::render(frame, state)))
        .expect("render workbench");
    let text = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    (text, plan.expect("layout plan"))
}

fn insert(text: impl Into<String>) -> Action {
    Action::Edit {
        target: EditorTarget::Composer,
        command: EditCommand::Insert {
            text: text.into(),
            typing: false,
        },
    }
}

#[test]
fn slash_new_creates_directly_without_a_dialog_and_cannot_be_submitted_twice() {
    let workspace = WorkspaceId::new();
    let info = session(workspace, "existing");
    let mut state = opened_state(&[info]);
    update(&mut state, UiEvent::Action(insert("/new")));

    let first = update(&mut state, UiEvent::Action(Action::Submit));
    assert!(
        matches!(first.as_slice(), [Effect::CreateSession { .. }]),
        "/new must create a session instead of becoming user input: {first:?}"
    );
    let Effect::CreateSession { request_id, .. } = first[0] else {
        unreachable!()
    };
    assert_eq!(
        state
            .pending_create
            .as_ref()
            .map(|pending| pending.request_id),
        Some(request_id),
        "the in-flight creation identity must be retained"
    );

    let second = update(&mut state, UiEvent::Action(Action::Submit));
    assert!(
        second.is_empty(),
        "repeated Enter must retain one in-flight creation identity: {second:?}"
    );
}

#[test]
fn text_typed_before_any_session_exists_is_kept_visible() {
    let mut state = UiState::default();
    state.workspace_label = Some("empty workspace".into());
    update(
        &mut state,
        UiEvent::Action(insert("a normal first request")),
    );

    let (screen, _) = render(&state, 140, 36);
    assert!(
        screen.contains("a normal first request"),
        "the empty-workspace composer must retain ordinary text"
    );
}

#[test]
fn unknown_slash_command_is_not_sent_as_a_product_request() {
    let workspace = WorkspaceId::new();
    let info = session(workspace, "commands");
    let mut state = opened_state(&[info]);
    update(
        &mut state,
        UiEvent::Action(insert("/definitely-not-a-command")),
    );

    let effects = update(&mut state, UiEvent::Action(Action::Submit));
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, Effect::Submit { .. })),
        "unknown slash text must never silently become a normal submission"
    );
    assert_eq!(
        state.selected_ui().expect("selected session").draft(),
        "/definitely-not-a-command",
        "rejected command text remains recoverable"
    );
}

#[test]
fn drafts_and_submission_receipts_remain_bound_to_their_session_and_identity() {
    let workspace = WorkspaceId::new();
    let first = session(workspace, "first");
    let second = session(workspace, "second");
    let mut state = opened_state(&[first.clone(), second.clone()]);

    update(&mut state, UiEvent::Action(insert("first draft")));
    update(
        &mut state,
        UiEvent::Action(Action::SelectSession(second.id)),
    );
    update(&mut state, UiEvent::Action(Action::Focus(Focus::Composer)));
    update(&mut state, UiEvent::Action(insert("second draft")));
    let generation = state.session_ui[&second.id].generation;
    let submitted = update(&mut state, UiEvent::Action(Action::Submit));
    let request_id = submitted
        .iter()
        .find_map(|effect| match effect {
            Effect::Submit { session, input } if *session == second.id => Some(input.request_id),
            _ => None,
        })
        .expect("second-session submit");

    update(
        &mut state,
        UiEvent::Submitted {
            session: second.id,
            request_id: RequestId::new(),
        },
    );
    assert_eq!(state.session_ui[&first.id].draft(), "first draft");
    assert_eq!(state.session_ui[&second.id].draft(), "second draft");

    update(
        &mut state,
        UiEvent::Submitted {
            session: second.id,
            request_id,
        },
    );
    assert_eq!(state.session_ui[&first.id].draft(), "first draft");
    assert!(state.session_ui[&second.id].draft().is_empty());

    update(
        &mut state,
        UiEvent::SessionOpened {
            session: first.id,
            generation: generation.wrapping_add(100),
            snapshot: snapshot(&first, "stale server draft"),
            history: RecentHistoryPage {
                items: Vec::new(),
                older_cursor: None,
                snapshot_through: bone_app::SessionSeq(0),
            },
        },
    );
    assert_eq!(
        state.session_ui[&first.id].draft(),
        "first draft",
        "a stale open response cannot overwrite a local per-session draft"
    );
}

#[test]
fn public_focus_actions_restore_the_center_region_the_user_left() {
    let workspace = WorkspaceId::new();
    let info = session(workspace, "focus");
    let mut state = opened_state(&[info]);
    assert_eq!(state.focus, Focus::Composer);

    update(&mut state, UiEvent::Action(Action::FocusLeft));
    assert_eq!(state.focus, Focus::Sessions);
    update(&mut state, UiEvent::Action(Action::FocusRight));
    assert_eq!(state.focus, Focus::Composer);

    update(&mut state, UiEvent::Action(Action::FocusUp));
    assert_eq!(state.focus, Focus::SessionTitle);
    update(&mut state, UiEvent::Action(Action::FocusLeft));
    assert_eq!(state.focus, Focus::Sessions);
    update(&mut state, UiEvent::Action(Action::FocusRight));
    assert_eq!(state.focus, Focus::SessionTitle);
    update(&mut state, UiEvent::Action(Action::FocusDown));
    assert_eq!(state.focus, Focus::Composer);
}

#[test]
fn composer_geometry_is_contained_by_the_center_surface() {
    for (width, height) in [(180, 44), (125, 30), (100, 30), (90, 24)] {
        let plan = crate::layout::LayoutPlan::calculate_with_widths(
            Rect::new(0, 0, width, height),
            SinglePane::Conversation,
            2,
            None,
            None,
            1,
            crate::layout::PaneWidths::default(),
        );
        let center = plan.conversation.expect("conversation area");
        let composer = plan.composer.expect("composer area");
        assert!(contained_by(composer, center));
    }
}

#[test]
fn right_rail_is_an_explicit_focus_target_without_phantom_actions() {
    let (_, plan) = render(&UiState::default(), 180, 44);
    let blank = plan
        .layout
        .extension_blank
        .expect("wide layout blank extension");
    assert_eq!(
        plan.hit(blank.x, blank.y),
        Some(HitTarget::PaneDivider(crate::layout::PaneDivider::Right))
    );
    for y in blank.y..blank.bottom() {
        for x in blank.x.saturating_add(1)..blank.right() {
            assert_eq!(
                plan.hit(x, y),
                Some(HitTarget::Action(Action::Focus(Focus::RightRail))),
                "the visible right rail must have one stable focus target at ({x}, {y})"
            );
        }
    }
}

#[test]
fn conversation_rendering_has_no_bone_or_you_speaker_prefixes() {
    let workspace = WorkspaceId::new();
    let info = session(workspace, "prefix contract");
    let mut state = opened_state(std::slice::from_ref(&info));
    let ui = state.session_ui.get_mut(&info.id).unwrap();
    ui.transcript.open(RecentHistoryPage {
        items: vec![
            HistoryEntry {
                sequence: bone_app::SessionSeq(1),
                occurred_at: 1,
                event: SessionEvent::InputSubmitted {
                    input: bone_app::InputId(1),
                    request_id: RequestId::new(),
                    text: "UNIQUE_USER_BODY".into(),
                    reply_to: None,
                },
            },
            HistoryEntry {
                sequence: bone_app::SessionSeq(2),
                occurred_at: 2,
                event: SessionEvent::Reply {
                    job: bone_app::JobRef {
                        runtime: bone_app::RuntimeId::new(),
                        id: 1,
                    },
                    inputs: vec![bone_app::InputId(1)],
                    text: "UNIQUE_ASSISTANT_BODY".into(),
                },
            },
        ],
        older_cursor: None,
        snapshot_through: bone_app::SessionSeq(2),
    });
    let (screen, _) = render(&state, 160, 40);
    assert!(screen.contains("UNIQUE_USER_BODY"));
    assert!(screen.contains("UNIQUE_ASSISTANT_BODY"));
    assert!(!screen.contains("BONE  UNIQUE_ASSISTANT_BODY"));
    assert!(!screen.contains("YOU  UNIQUE_USER_BODY"));
    assert!(!screen.contains("你  UNIQUE_USER_BODY"));
}

#[test]
fn external_terminal_controls_and_bidi_overrides_are_removed() {
    let hostile = "safe\u{1b}]8;;https://invalid.example\u{7}link\u{1b}\\\u{202e}txt\r\0\nnext";
    let sanitized = view::sanitize_external(hostile);
    assert_eq!(sanitized, "safe]8;;https://invalid.examplelink\\txt\nnext");
    assert!(!sanitized.chars().any(|character| {
        character == '\u{1b}'
            || character == '\u{7}'
            || character == '\r'
            || character == '\0'
            || matches!(character as u32, 0x202a..=0x202e | 0x2066..=0x2069)
    }));
}

fn contained_by(inner: Rect, outer: Rect) -> bool {
    inner.x >= outer.x
        && inner.y >= outer.y
        && inner.right() <= outer.right()
        && inner.bottom() <= outer.bottom()
}

#[test]
fn large_drafts_preserve_history_space_and_padding() {
    for (w, h) in [(40, 12), (80, 24), (120, 30), (160, 40)] {
        let mut state = UiState::default();
        update(
            &mut state,
            UiEvent::Action(insert("中文草稿\n".repeat(100))),
        );
        let (_, plan) = render(&state, w, h);
        assert!(plan.layout.transcript.unwrap().height >= 3);
        assert!(plan.layout.composer.unwrap().height <= 9);
    }
}
#[test]
fn minimum_menu_scrolls_to_selected_command_and_hits_it() {
    let mut state = UiState::default();
    update(&mut state, UiEvent::Action(insert("/")));
    for index in 0..state.slash_matches().len() {
        state.slash_selection = index;
        let (screen, plan) = render(&state, 40, 12);
        let command = state.slash_matches()[index];
        assert!(screen.contains(command.name));
        assert!(
            plan.hit_regions()
                .iter()
                .any(|r| r.target == HitTarget::Action(Action::ExecuteCommand(command.kind)))
        );
    }
}
#[test]
fn submission_receipt_survives_leaving_and_reopening_its_session() {
    let workspace = WorkspaceId::new();
    let first = session(workspace, "first");
    let second = session(workspace, "second");
    let mut state = opened_state(&[first.clone(), second.clone()]);
    update(&mut state, UiEvent::Action(insert("original")));
    let effects = update(&mut state, UiEvent::Action(Action::Submit));
    let request_id = effects
        .iter()
        .find_map(|e| match e {
            Effect::Submit { input, .. } => Some(input.request_id),
            _ => None,
        })
        .unwrap();
    update(
        &mut state,
        UiEvent::Action(Action::SelectSession(second.id)),
    );
    update(&mut state, UiEvent::Action(Action::SelectSession(first.id)));
    update(&mut state, UiEvent::Action(Action::Focus(Focus::Composer)));
    update(&mut state, UiEvent::Action(insert(" plus new edit")));
    update(
        &mut state,
        UiEvent::Submitted {
            session: first.id,
            request_id,
        },
    );
    assert!(state.selected_ui().unwrap().submitting.is_none());
    assert_eq!(state.draft(), "original plus new edit");
    assert!(
        update(&mut state, UiEvent::Action(Action::Submit))
            .iter()
            .any(|e| matches!(e, Effect::Submit { .. }))
    );
}
#[test]
fn too_small_has_no_invisible_pointer_actions() {
    let mut state = UiState::default();
    update(&mut state, UiEvent::Action(insert("unsent text")));
    for (w, h) in [(39, 12), (40, 11)] {
        let (screen, plan) = render(&state, w, h);
        assert!(screen.contains("Window too small"));
        assert!(plan.hit_regions().is_empty());
    }
}

#[test]
fn comfortable_session_targets_include_padding_but_exclude_inter_item_gaps() {
    let workspace = WorkspaceId::new();
    let state = opened_state(&[session(workspace, "first"), session(workspace, "second")]);
    let first = state.session_rows[0].id();
    for height in [12, 23, 24, 40] {
        let (_, plan) = render(&state, 120, height);
        let row = plan
            .hit_regions()
            .iter()
            .find(|hit| hit.target == HitTarget::Session(first))
            .unwrap();
        assert_eq!(row.area.height, 3);
        for y in row.area.y..row.area.bottom() {
            assert_eq!(plan.hit(row.area.x + 2, y), Some(HitTarget::Session(first)));
        }
        assert_eq!(
            plan.hit(row.area.x + 2, row.area.bottom()),
            Some(HitTarget::SessionRail)
        );
        if height >= 24 {
            assert_eq!(plan.layout.composer.unwrap().height, 6);
        }
        assert!(plan.layout.transcript.unwrap().height >= 3);
    }
}
