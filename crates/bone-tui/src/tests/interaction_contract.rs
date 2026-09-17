use std::sync::Arc;

use bone_app::{
    HistoryEntry, RecentHistoryPage, SessionEvent, SessionId, SessionInfo, SessionSeq, SessionView,
};
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::{Terminal, backend::TestBackend, layout::Rect};

use crate::{
    input::terminal_event,
    layout::{ClickTarget, PaneDivider},
    state::{
        Action, Effect, KeyboardOwner, ModelChoice, ModelPanel, ModelScreen, Overlay,
        PointerCapture, SessionNavRow, SessionUi, UiEvent, UiState, WorkspaceTarget, update,
    },
    ui::interaction::ScrollTarget,
    view::{self, FrameSnapshot},
};

fn fixture() -> UiState {
    let workspace = bone_app::WorkspaceId::new();
    let sessions: Vec<_> = (0..60)
        .map(|index| SessionInfo {
            id: SessionId::new(),
            workspace,
            title: format!("session {index}"),
            archived: false,
        })
        .collect();
    let info = sessions[0].clone();
    let mut state = UiState::default();
    state.session_rows = sessions
        .into_iter()
        .map(SessionNavRow::provisional)
        .collect();
    state.selected = Some(info.id);
    let mut ui = SessionUi::new(info.id, 1);
    ui.hydrated = true;
    ui.draft = "replace keep".into();
    ui.snapshot = Some(Arc::new(SessionView {
        session: info.clone(),
        runtime: bone_app::RuntimeState::Detached,
        draft: String::new(),
        inputs: vec![],
        jobs: vec![],
        activity: vec![],
        history_through: SessionSeq(1),
        problem: None,
    }));
    ui.transcript.open(RecentHistoryPage {
        items: vec![HistoryEntry {
            sequence: SessionSeq(1),
            occurred_at: 0,
            event: SessionEvent::ToolFinished {
                call: bone_app::CallRef {
                    runtime: bone_app::RuntimeId::new(),
                    id: 1,
                },
                job: bone_app::JobRef {
                    runtime: bone_app::RuntimeId::new(),
                    id: 1,
                },
                tool: "read".into(),
                arguments: serde_json::json!({"path": "result.txt"}),
                outcome: bone_app::ToolOutcome::value(serde_json::json!({
                    "lines": (0..100).map(|line| format!("result line {line}")).collect::<Vec<_>>()
                })),
            },
        }],
        older_cursor: None,
        snapshot_through: SessionSeq(1),
    });
    state.session_ui.insert(info.id, ui);
    state
}

fn chatgpt_add_panel(state: &mut UiState) {
    let mut models = ModelPanel::new(state.selected);
    models.screen = ModelScreen::Add { selected: 0 };
    models.profiles.push(bone_app::Profile::chatgpt());
    models.choices.push(ModelChoice {
        selection: bone_app::ModelSelection::new(bone_app::ProfileId::chatgpt(), "gpt-test")
            .unwrap(),
        profile_label: "ChatGPT".into(),
        label: "GPT Test".into(),
    });
    state.overlay = Some(Overlay::Models(models));
}

fn responses_model_panel(state: &mut UiState) {
    let mut profile = bone_app::Profile::new(
        bone_app::ProfileId::new("test-api").unwrap(),
        "Test API",
        bone_app::EndpointConfig::OpenAiResponses {
            base_url: Some("http://127.0.0.1:8080/v1".into()),
        },
    )
    .unwrap();
    profile.add_model("gpt-test").unwrap();
    let mut models = ModelPanel::new(state.selected);
    models.profiles.push(profile.clone());
    models.choices.push(ModelChoice {
        selection: bone_app::ModelSelection::new(profile.id, "gpt-test").unwrap(),
        profile_label: profile.label,
        label: "gpt-test".into(),
    });
    state.overlay = Some(Overlay::Models(models));
}

fn render(state: &mut UiState, width: u16, height: u16) -> (String, FrameSnapshot) {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    let mut snapshot = None;
    terminal
        .draw(|frame| snapshot = Some(view::render(frame, state)))
        .unwrap();
    let snapshot = snapshot.unwrap();
    if let Some(metrics) = snapshot.transcript_metrics.clone() {
        crate::state::retain_transcript(state, metrics);
    }
    let text = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    (text, snapshot)
}

fn dispatch(state: &mut UiState, snapshot: &FrameSnapshot, event: Event) {
    if let Event::Mouse(mouse) = &event {
        update(
            state,
            UiEvent::PointerMoved {
                column: mouse.column,
                row: mouse.row,
            },
        );
    }
    let event = terminal_event(event, Some(snapshot), state).expect("operable target");
    update(state, event);
}

fn click(state: &mut UiState, snapshot: &FrameSnapshot, area: Rect) {
    dispatch(
        state,
        snapshot,
        mouse(MouseEventKind::Down(MouseButton::Left), area.x, area.y),
    );
    dispatch(
        state,
        snapshot,
        mouse(MouseEventKind::Up(MouseButton::Left), area.x, area.y),
    );
}

fn mouse(kind: MouseEventKind, x: u16, y: u16) -> Event {
    Event::Mouse(MouseEvent {
        kind,
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    })
}

fn click_action(snapshot: &FrameSnapshot, action: Action) -> Rect {
    snapshot
        .hit_regions()
        .into_iter()
        .find(|region| region.target == ClickTarget::Action(action.clone()))
        .expect("visible action")
        .area
}

#[test]
fn real_history_details_keep_all_scroll_regions_dividers_and_selected_input_operable() {
    let mut state = fixture();
    let (_, mut snapshot) = render(&mut state, 160, 40);
    // Bring the long result's actual link into view through the real scroll route.
    for _ in 0..100 {
        if snapshot
            .hit_regions()
            .into_iter()
            .any(|region| region.target == ClickTarget::Action(Action::OpenHistory(SessionSeq(1))))
        {
            break;
        }
        let area = snapshot.layout.transcript.unwrap();
        dispatch(
            &mut state,
            &snapshot,
            mouse(MouseEventKind::ScrollUp, area.x + 2, area.y + 2),
        );
        snapshot = render(&mut state, 160, 40).1;
    }
    let result = click_action(&snapshot, Action::OpenHistory(SessionSeq(1)));
    assert!(matches!(
        terminal_event(
            mouse(MouseEventKind::ScrollDown, result.x, result.y),
            Some(&snapshot),
            &state
        ),
        Some(UiEvent::Action(Action::ScrollDown(3)))
    ));
    dispatch(
        &mut state,
        &snapshot,
        mouse(MouseEventKind::ScrollDown, result.x, result.y),
    );
    snapshot = render(&mut state, 160, 40).1;
    let transcript = snapshot.layout.transcript.unwrap();
    dispatch(
        &mut state,
        &snapshot,
        mouse(MouseEventKind::ScrollUp, transcript.x + 2, transcript.y + 2),
    );
    snapshot = render(&mut state, 160, 40).1;
    let result = click_action(&snapshot, Action::OpenHistory(SessionSeq(1)));
    dispatch(
        &mut state,
        &snapshot,
        Event::Key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE)),
    );
    for _ in 0..7 {
        dispatch(
            &mut state,
            &snapshot,
            Event::Key(KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT)),
        );
    }
    assert_eq!(state.editor().selection(), Some(0..7));
    click(&mut state, &snapshot, result);
    assert!(state.details.is_some());
    snapshot = render(&mut state, 160, 40).1;
    let left = snapshot.layout.session_rail.unwrap();
    let middle = snapshot.layout.transcript.unwrap();
    let right = snapshot.layout.details_area().unwrap();
    for (area, target, direction) in [
        (left, ScrollTarget::Sessions, MouseEventKind::ScrollDown),
        (middle, ScrollTarget::Conversation, MouseEventKind::ScrollUp),
        (right, ScrollTarget::Details, MouseEventKind::ScrollDown),
    ] {
        assert_eq!(snapshot.scroll_hit(area.x + 2, area.y + 2), Some(target));
        dispatch(
            &mut state,
            &snapshot,
            mouse(direction, area.x + 2, area.y + 2),
        );
    }
    assert!(state.session_scroll.is_some_and(|start| start > 0));
    assert!(state.details.as_ref().unwrap().scroll > 0);
    assert_eq!(state.editor().selection(), Some(0..7));

    snapshot = render(&mut state, 160, 40).1;
    let models = click_action(&snapshot, Action::OpenModels);
    click(&mut state, &snapshot, models);
    for divider in [PaneDivider::Left, PaneDivider::Right] {
        snapshot = render(&mut state, 160, 40).1;
        let edge = snapshot
            .hit_regions()
            .into_iter()
            .find(|region| region.target == ClickTarget::PaneDivider(divider))
            .unwrap()
            .area;
        let overlay = snapshot.overlay_area().unwrap();
        dispatch(
            &mut state,
            &snapshot,
            mouse(MouseEventKind::Down(MouseButton::Left), edge.x, 0),
        );
        assert_eq!(
            state.pointer.capture,
            Some(PointerCapture::Divider(divider))
        );
        dispatch(
            &mut state,
            &snapshot,
            mouse(
                MouseEventKind::Drag(MouseButton::Left),
                overlay.x + 1,
                overlay.y + 1,
            ),
        );
        assert_eq!(
            state.pointer.capture,
            Some(PointerCapture::Divider(divider))
        );
        dispatch(
            &mut state,
            &snapshot,
            mouse(
                MouseEventKind::Up(MouseButton::Left),
                overlay.x + 1,
                overlay.y + 1,
            ),
        );
        assert_eq!(state.pointer.capture, None);
    }
    snapshot = render(&mut state, 160, 40).1;
    assert_eq!(state.editor().selection(), Some(0..7));
    dispatch(
        &mut state,
        &snapshot,
        Event::Key(KeyEvent::new(KeyCode::Char('X'), KeyModifiers::NONE)),
    );
    assert_eq!(state.draft(), "X keep");
    assert_eq!(
        state.keyboard,
        KeyboardOwner::Workspace(WorkspaceTarget::Composer)
    );
}

#[test]
fn mouse_model_overlays_keep_input_visible_and_controls_within_their_surface() {
    for (width, height) in [(40, 12), (80, 24), (160, 40)] {
        let mut state = fixture();
        let (_, snapshot) = render(&mut state, width, height);
        let button = click_action(&snapshot, Action::OpenModels);
        click(&mut state, &snapshot, button);
        let (_, snapshot) = render(&mut state, width, height);
        let overlay = snapshot.overlay_area().unwrap();
        let composer = snapshot.layout.composer.unwrap();
        assert!(overlay.intersection(composer).is_empty());
        for region in snapshot.hit_regions().into_iter().filter(|region| {
            matches!(
                region.target,
                ClickTarget::Action(
                    Action::SelectModel(_)
                        | Action::ChooseConnection(_)
                        | Action::CloseOverlay
                        | Action::RetryLogin
                )
            )
        }) {
            assert_eq!(
                region.area.intersection(overlay),
                region.area,
                "{width}x{height}: {:?}",
                region.target
            );
        }
        dispatch(
            &mut state,
            &snapshot,
            Event::Key(KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE)),
        );
        let (text, _) = render(&mut state, width, height);
        assert_eq!(state.draft(), "replace keep!");
        assert!(text.contains("replace keep!"));
        assert_eq!(
            state.keyboard,
            KeyboardOwner::Workspace(WorkspaceTarget::Composer)
        );
    }
}

#[test]
fn model_panel_has_one_keyboard_truth_and_an_independent_pointer_route() {
    let mut state = fixture();
    responses_model_panel(&mut state);
    let (_, snapshot) = render(&mut state, 80, 24);
    let choice = click_action(&snapshot, Action::SelectModel(0));

    let down = terminal_event(
        mouse(MouseEventKind::Down(MouseButton::Left), choice.x, choice.y),
        Some(&snapshot),
        &state,
    )
    .unwrap();
    assert!(update(&mut state, down).is_empty());
    let up = terminal_event(
        mouse(MouseEventKind::Up(MouseButton::Left), choice.x, choice.y),
        Some(&snapshot),
        &state,
    )
    .unwrap();
    let effects = update(&mut state, up);
    assert!(effects.is_empty());
    let (_, snapshot) = render(&mut state, 80, 24);
    let reasoning = click_action(&snapshot, Action::SelectReasoning(3));
    let down = terminal_event(
        mouse(
            MouseEventKind::Down(MouseButton::Left),
            reasoning.x,
            reasoning.y,
        ),
        Some(&snapshot),
        &state,
    )
    .unwrap();
    assert!(update(&mut state, down).is_empty());
    let up = terminal_event(
        mouse(
            MouseEventKind::Up(MouseButton::Left),
            reasoning.x,
            reasoning.y,
        ),
        Some(&snapshot),
        &state,
    )
    .unwrap();
    let effects = update(&mut state, up);
    assert!(matches!(
        effects.as_slice(),
        [Effect::SetModel { selection, .. }] if selection.model == "gpt-test"
    ));
    assert_eq!(
        state.keyboard,
        KeyboardOwner::Workspace(WorkspaceTarget::Composer),
        "a pointer action must not acquire keyboard ownership"
    );

    let mut state = fixture();
    responses_model_panel(&mut state);
    let enter = terminal_event(
        Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        None,
        &state,
    )
    .unwrap();
    assert!(matches!(&enter, UiEvent::Action(Action::ActivatePanel)));
    let effects = update(&mut state, enter);
    assert!(effects.is_empty());
    let enter = terminal_event(
        Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        None,
        &state,
    )
    .unwrap();
    let effects = update(&mut state, enter);
    assert!(matches!(
        effects.as_slice(),
        [Effect::SetModel { selection, .. }] if selection.model == "gpt-test"
    ));
    assert!(state.keyboard.is_overlay());
    let (text, _) = render(&mut state, 80, 24);
    assert!(text.contains("Applying model…"));
}

#[test]
fn model_panel_selection_style_follows_keyboard_ownership() {
    let mut state = fixture();
    chatgpt_add_panel(&mut state);
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let mut snapshot = None;
    terminal
        .draw(|frame| snapshot = Some(view::render(frame, &state)))
        .unwrap();
    let choice = click_action(&snapshot.unwrap(), Action::ChooseConnection(0));
    assert_eq!(
        terminal.backend().buffer()[(choice.x, choice.y)].bg,
        crate::ui::theme::INPUT
    );

    state.enter_overlay();
    let mut active_snapshot = None;
    terminal
        .draw(|frame| {
            active_snapshot = Some(view::render(frame, &state));
        })
        .unwrap();
    let choice = click_action(&active_snapshot.unwrap(), Action::ChooseConnection(0));
    assert_eq!(
        terminal.backend().buffer()[(choice.x, choice.y)].bg,
        crate::ui::theme::SELECTED
    );
}

#[test]
fn small_login_and_help_content_can_scroll_to_the_last_line_without_taking_keyboard() {
    for overlay in [Overlay::Help, {
        let mut models = ModelPanel::new(None);
        models.screen = ModelScreen::Login {
            request: 1,
            state: bone_app::LoginState::DeviceCode {
                verification_uri: format!("https://example.test/{}", "long-path/".repeat(10)),
                user_code: "LAST-CODE".into(),
            },
        };
        Overlay::Models(models)
    }] {
        let mut state = fixture();
        let login = matches!(overlay, Overlay::Models(_));
        state.overlay = Some(overlay);
        let (_, mut snapshot) = render(&mut state, 40, 12);
        let mut max = 0;
        for _ in 0..100 {
            let area = snapshot.overlay_area().unwrap();
            let target = snapshot.scroll_hit(area.x + 1, area.y + 1);
            let Some(ScrollTarget::OverlayContent { max: limit }) = target else {
                panic!("scrollable content: {target:?}")
            };
            max = limit;
            if state.overlay_scroll == limit {
                break;
            }
            dispatch(
                &mut state,
                &snapshot,
                mouse(MouseEventKind::ScrollDown, area.x + 1, area.y + 1),
            );
            snapshot = render(&mut state, 40, 12).1;
        }
        assert!(max > 0);
        assert_eq!(state.overlay_scroll, max);
        let (text, _) = render(&mut state, 40, 12);
        if login {
            assert!(text.contains("Waiting for sign-in"), "{text}");
        } else {
            assert!(text.contains("Latest task / tool"), "{text}");
        }
        assert_eq!(
            state.keyboard,
            KeyboardOwner::Workspace(WorkspaceTarget::Composer)
        );
    }
}

#[test]
fn hidden_caret_reappears_for_form_edits_field_changes_and_keyboard_transitions() {
    let mut state = fixture();
    let mut models = ModelPanel::new(None);
    models.screen = ModelScreen::Setup(Box::new(crate::state::ConnectionForm::new(
        crate::state::ConnectionKind::CustomOpenAiResponses,
    )));
    state.overlay = Some(Overlay::Models(models));
    let (_, mut snapshot) = render(&mut state, 80, 24);
    update(&mut state, UiEvent::CaretBlink);
    assert!(!state.caret_visible);
    dispatch(
        &mut state,
        &snapshot,
        Event::Key(KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE)),
    );
    assert!(state.caret_visible, "entering the form reveals its caret");

    for event in [
        Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)),
        Event::Paste("pasted".into()),
        Event::Key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)),
        Event::Key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL)),
        Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)),
        Event::Key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT)),
    ] {
        update(&mut state, UiEvent::CaretBlink);
        assert!(!state.caret_visible);
        snapshot = render(&mut state, 80, 24).1;
        dispatch(&mut state, &snapshot, event);
        assert!(
            state.caret_visible,
            "a form edit or field change reveals the caret"
        );
    }

    update(&mut state, UiEvent::CaretBlink);
    assert!(!state.caret_visible);
    update(&mut state, UiEvent::PointerMoved { column: 1, row: 1 });
    update(
        &mut state,
        UiEvent::Action(Action::ScrollOverlay { amount: 3, max: 10 }),
    );
    dispatch(&mut state, &snapshot, Event::Paste("\n".into()));
    assert!(
        !state.caret_visible,
        "hover, scrolling and filtered text do not restart the caret"
    );
    dispatch(
        &mut state,
        &snapshot,
        Event::Key(KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE)),
    );
    assert!(
        state.caret_visible,
        "returning to the composer reveals its caret"
    );
    assert_eq!(
        state.keyboard,
        KeyboardOwner::Workspace(WorkspaceTarget::Composer)
    );
}

#[test]
fn preparing_commands_reveals_only_the_active_composer_caret() {
    let mut state = UiState::default();
    update(&mut state, UiEvent::CaretBlink);
    assert!(!state.caret_visible);
    update(&mut state, UiEvent::Action(Action::StartSlashCommand));
    assert_eq!(state.draft(), "/");
    assert!(state.caret_visible);
    update(&mut state, UiEvent::CaretBlink);
    update(&mut state, UiEvent::Action(Action::CompleteSlash));
    assert!(state.caret_visible);
    update(&mut state, UiEvent::CaretBlink);
    update(
        &mut state,
        UiEvent::Action(Action::PrepareCommand(crate::state::CommandKind::Help)),
    );
    assert_eq!(state.draft(), "/help");
    assert!(state.caret_visible);

    let mut models = ModelPanel::new(None);
    models.screen = ModelScreen::Setup(Box::new(crate::state::ConnectionForm::new(
        crate::state::ConnectionKind::OpenAiApi,
    )));
    state.overlay = Some(Overlay::Models(models));
    state.enter_overlay();
    update(&mut state, UiEvent::CaretBlink);
    assert!(!state.caret_visible);
    update(
        &mut state,
        UiEvent::Action(Action::PrepareCommand(crate::state::CommandKind::Model)),
    );
    assert_eq!(state.draft(), "/model");
    assert!(
        !state.caret_visible,
        "changing a background draft does not reveal the active form caret"
    );
}
