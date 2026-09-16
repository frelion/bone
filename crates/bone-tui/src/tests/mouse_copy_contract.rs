use std::sync::Arc;

use bone_app::{
    HistoryEntry, RecentHistoryPage, SessionEvent, SessionId, SessionInfo, SessionSeq, SessionView,
};
use crossterm::event::{Event, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend};

use crate::{
    editor::EditCommand,
    input::terminal_event,
    layout::ClickTarget,
    state::{Action, EditorTarget, Effect, SessionNavRow, SessionUi, UiEvent, UiState, update},
    ui::selection::CopySource,
    view::{self, FrameSnapshot},
};

fn fixture(event: SessionEvent) -> UiState {
    let info = SessionInfo {
        id: SessionId::new(),
        workspace: bone_app::WorkspaceId::new(),
        title: "Copy contract".into(),
        archived: false,
    };
    let mut state = UiState::default();
    state.session_rows = vec![SessionNavRow::provisional(info.clone())];
    state.selected = Some(info.id);
    let mut ui = SessionUi::new(info.id, 1);
    ui.hydrated = true;
    ui.draft = "ab界cd".into();
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
            event,
        }],
        older_cursor: None,
        snapshot_through: SessionSeq(1),
    });
    state.session_ui.insert(info.id, ui);
    state
}

fn reply(text: &str) -> UiState {
    fixture(SessionEvent::Reply {
        job: bone_app::JobRef {
            runtime: bone_app::RuntimeId::new(),
            id: 1,
        },
        inputs: vec![],
        text: text.into(),
    })
}

fn tool() -> UiState {
    let runtime = bone_app::RuntimeId::new();
    fixture(SessionEvent::ToolFinished {
        call: bone_app::CallRef { runtime, id: 1 },
        job: bone_app::JobRef { runtime, id: 1 },
        tool: "read".into(),
        outcome: bone_app::ToolOutcome {
            result: Ok(
                serde_json::json!({"lines": (0..100).map(|n| format!("line {n}: 中文 e\u{301}")).collect::<Vec<_>>()}),
            ),
            external_effect: bone_app::ExternalEffect::None,
        },
    })
}

fn draw(state: &mut UiState, width: u16, height: u16) -> FrameSnapshot {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    let mut snapshot = None;
    terminal
        .draw(|frame| snapshot = Some(view::render(frame, state)))
        .unwrap();
    let snapshot = snapshot.unwrap();
    if let Some(metrics) = snapshot.transcript_metrics.clone() {
        crate::state::retain_transcript(state, metrics);
    }
    snapshot
}

fn mouse(
    state: &mut UiState,
    frame: &FrameSnapshot,
    kind: MouseEventKind,
    pos: (u16, u16),
) -> Vec<Effect> {
    update(
        state,
        UiEvent::PointerMoved {
            column: pos.0,
            row: pos.1,
        },
    );
    terminal_event(
        Event::Mouse(MouseEvent {
            kind,
            column: pos.0,
            row: pos.1,
            modifiers: KeyModifiers::NONE,
        }),
        Some(frame),
        state,
    )
    .map_or_else(Vec::new, |event| update(state, event))
}

fn copied(effects: Vec<Effect>) -> Vec<String> {
    effects
        .into_iter()
        .filter_map(|effect| {
            if let Effect::CopyText(text) = effect {
                Some(text)
            } else {
                None
            }
        })
        .collect()
}

fn position(frame: &FrameSnapshot, source: CopySource, byte: usize) -> (u16, u16) {
    let area = frame.layout.screen;
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            if frame
                .text_at(x, y)
                .is_some_and(|point| point.source == source && point.byte == byte)
            {
                return (x, y);
            }
        }
    }
    panic!("source byte {byte} is not visible: {source:?}");
}

fn transcript(state: &UiState) -> CopySource {
    CopySource::Transcript(state.selected.unwrap())
}

#[test]
fn reply_drag_copies_original_code_in_both_directions_without_touching_the_editor() {
    let body = "    let 标识 = \"abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ\";\n\tprintln!(\"中文 e\u{301} 👩‍💻\");\nend";
    for reverse in [false, true] {
        let mut state = reply(body);
        for (byte, extend) in [(1, false), (5, true)] {
            update(
                &mut state,
                UiEvent::Action(Action::Edit {
                    target: EditorTarget::Composer,
                    command: EditCommand::Point { byte, extend },
                }),
            );
        }
        let owner = state.keyboard;
        let frame = draw(&mut state, 80, 30);
        let start = position(&frame, transcript(&state), 0);
        let end = position(&frame, transcript(&state), body.len());
        assert!(
            end.1 > start.1 + 2,
            "the long code line must actually soft-wrap"
        );
        let (start, end) = if reverse { (end, start) } else { (start, end) };
        assert!(
            copied(mouse(
                &mut state,
                &frame,
                MouseEventKind::Down(MouseButton::Left),
                start
            ))
            .is_empty()
        );
        mouse(
            &mut state,
            &frame,
            MouseEventKind::Drag(MouseButton::Left),
            end,
        );
        let frame = draw(&mut state, 80, 30);
        assert_eq!(
            copied(mouse(
                &mut state,
                &frame,
                MouseEventKind::Up(MouseButton::Left),
                end
            )),
            [body]
        );
        assert_eq!(state.keyboard, owner);
        assert_eq!(state.draft_cursor(), 5);
        assert_eq!(state.editor().selection(), Some(1..5));
        assert_eq!(state.draft(), "ab界cd");
    }
}

#[test]
fn dragging_a_tool_link_copies_its_label_and_only_a_plain_release_opens_details() {
    let mut state = tool();
    let frame = draw(&mut state, 160, 40);
    let start = position(&frame, transcript(&state), 0);
    let end = position(&frame, transcript(&state), 4);
    assert!(matches!(
        frame.hit(start.0, start.1),
        Some(ClickTarget::Action(Action::OpenHistory(_)))
    ));
    mouse(
        &mut state,
        &frame,
        MouseEventKind::Down(MouseButton::Left),
        start,
    );
    assert!(state.details.is_none());
    mouse(
        &mut state,
        &frame,
        MouseEventKind::Drag(MouseButton::Left),
        end,
    );
    assert_eq!(
        copied(mouse(
            &mut state,
            &frame,
            MouseEventKind::Up(MouseButton::Left),
            end
        )),
        ["read"]
    );
    assert!(state.details.is_none());
    mouse(
        &mut state,
        &frame,
        MouseEventKind::Down(MouseButton::Left),
        start,
    );
    assert!(state.details.is_none());
    assert!(
        copied(mouse(
            &mut state,
            &frame,
            MouseEventKind::Up(MouseButton::Left),
            start
        ))
        .is_empty()
    );
    assert!(state.details.is_some());
}

#[test]
fn editor_release_uses_its_final_coordinate_and_plain_click_does_not_copy() {
    let mut state = reply("body");
    let frame = draw(&mut state, 80, 24);
    let input = crate::layout::composer_text_area(frame.layout.composer.unwrap());
    // The rendered text is "ab界cd": b starts at column 1 and the wide 界
    // ends at column 4. Deliberately release without a final Drag event.
    let start = (input.x + 1, input.y);
    let end = (input.x + 4, input.y);
    mouse(
        &mut state,
        &frame,
        MouseEventKind::Down(MouseButton::Left),
        start,
    );
    assert_eq!(
        copied(mouse(
            &mut state,
            &frame,
            MouseEventKind::Up(MouseButton::Left),
            end
        )),
        ["b界"]
    );
    assert_eq!(state.editor().selection(), Some(1..5));
    mouse(
        &mut state,
        &frame,
        MouseEventKind::Down(MouseButton::Left),
        start,
    );
    assert!(
        copied(mouse(
            &mut state,
            &frame,
            MouseEventKind::Up(MouseButton::Left),
            start
        ))
        .is_empty()
    );
    assert_eq!(state.editor().selection(), None);
}

#[test]
fn dragging_back_to_the_anchor_clears_the_visible_selection_and_does_not_copy() {
    let mut state = reply("alpha beta");
    let frame = draw(&mut state, 80, 24);
    let start = position(&frame, transcript(&state), 0);
    let end = position(&frame, transcript(&state), 5);
    mouse(
        &mut state,
        &frame,
        MouseEventKind::Down(MouseButton::Left),
        start,
    );
    mouse(
        &mut state,
        &frame,
        MouseEventKind::Drag(MouseButton::Left),
        end,
    );
    assert_ne!(
        state.pointer.selection.as_ref().unwrap().anchor,
        state.pointer.selection.as_ref().unwrap().end
    );
    mouse(
        &mut state,
        &frame,
        MouseEventKind::Drag(MouseButton::Left),
        start,
    );
    let selection = state.pointer.selection.as_ref().unwrap();
    assert_eq!(
        selection.anchor, selection.end,
        "dragging back must remove the old highlight before release"
    );
    assert!(
        copied(mouse(
            &mut state,
            &frame,
            MouseEventKind::Up(MouseButton::Left),
            start
        ))
        .is_empty()
    );
}

#[test]
fn reader_selection_keeps_source_offsets_while_the_viewport_scrolls() {
    let mut state = tool();
    update(
        &mut state,
        UiEvent::Action(Action::OpenHistory(SessionSeq(1))),
    );
    let mut frame = draw(&mut state, 160, 40);
    let area = frame.layout.details_area().unwrap();
    let start = (area.x..area.right())
        .flat_map(|x| (area.y..area.bottom()).map(move |y| (x, y)))
        .find(|&(x, y)| {
            frame
                .text_at(x, y)
                .is_some_and(|p| matches!(p.source, CopySource::Details { .. }))
        })
        .unwrap();
    let anchor = frame.text_at(start.0, start.1).unwrap();
    let original = frame.source_content(anchor).unwrap();
    mouse(
        &mut state,
        &frame,
        MouseEventKind::Down(MouseButton::Left),
        start,
    );
    for _ in 0..4 {
        mouse(&mut state, &frame, MouseEventKind::ScrollDown, start);
        frame = draw(&mut state, 160, 40);
    }
    let end = (area.y..area.bottom())
        .rev()
        .flat_map(|y| (area.x..area.right()).rev().map(move |x| (x, y)))
        .find(|&(x, y)| {
            frame
                .text_at(x, y)
                .is_some_and(|p| p.source == anchor.source)
        })
        .unwrap();
    let endpoint = frame.text_at(end.0, end.1).unwrap();
    assert!(endpoint.byte > anchor.byte);
    mouse(
        &mut state,
        &frame,
        MouseEventKind::Drag(MouseButton::Left),
        end,
    );
    assert_eq!(
        copied(mouse(
            &mut state,
            &frame,
            MouseEventKind::Up(MouseButton::Left),
            end
        )),
        [&original[anchor.byte..endpoint.byte]]
    );
}

#[test]
fn smallest_workspace_can_copy_visible_text_and_an_overlay_does_not_select_behind_it() {
    let mut state = reply("中e\u{301}文");
    let frame = draw(&mut state, 40, 12);
    let start = position(&frame, transcript(&state), 0);
    let end = position(&frame, transcript(&state), "中e\u{301}文".len());
    mouse(
        &mut state,
        &frame,
        MouseEventKind::Down(MouseButton::Left),
        start,
    );
    mouse(
        &mut state,
        &frame,
        MouseEventKind::Drag(MouseButton::Left),
        end,
    );
    assert_eq!(
        copied(mouse(
            &mut state,
            &frame,
            MouseEventKind::Up(MouseButton::Left),
            end
        )),
        ["中e\u{301}文"]
    );
    let mut state = reply(&"covered text\n".repeat(40));
    let before_overlay = draw(&mut state, 80, 30);
    update(&mut state, UiEvent::Action(Action::OpenModels));
    let frame = draw(&mut state, 80, 30);
    let overlay = frame.overlay_area().unwrap();
    let blank = (overlay.y..overlay.bottom())
        .flat_map(|y| (overlay.x..overlay.right()).map(move |x| (x, y)))
        .find(|&(x, y)| frame.hit(x, y).is_none() && before_overlay.text_at(x, y).is_some())
        .unwrap();
    assert!(frame.text_at(blank.0, blank.1).is_none());
    let end = (blank.0.saturating_add(2).min(overlay.right() - 1), blank.1);
    mouse(
        &mut state,
        &frame,
        MouseEventKind::Down(MouseButton::Left),
        blank,
    );
    mouse(
        &mut state,
        &frame,
        MouseEventKind::Drag(MouseButton::Left),
        end,
    );
    assert!(
        copied(mouse(
            &mut state,
            &frame,
            MouseEventKind::Up(MouseButton::Left),
            end
        ))
        .is_empty()
    );
}

#[test]
fn login_url_copies_without_visual_wraps_or_taking_the_keyboard() {
    let mut state = reply("background");
    let uri = format!("https://example.invalid/{}", "segment/".repeat(15));
    let text = format!("Open in your browser:\n{uri}\n\nCode: ABCD-1234\nWaiting for sign-in…");
    let mut models = crate::state::ModelPanel::new(state.selected);
    models.screen = crate::state::ModelScreen::Login {
        request: 1,
        state: bone_app::LoginState::DeviceCode {
            verification_uri: uri.clone(),
            user_code: "ABCD-1234".into(),
        },
    };
    state.overlay = Some(crate::state::Overlay::Models(models));
    let owner = state.keyboard;
    let frame = draw(&mut state, 80, 24);
    let from = text.find(&uri).unwrap();
    let start = position(&frame, CopySource::Overlay, from);
    let end = position(&frame, CopySource::Overlay, from + uri.len());
    assert!(end.1 > start.1);
    mouse(
        &mut state,
        &frame,
        MouseEventKind::Down(MouseButton::Left),
        start,
    );
    mouse(
        &mut state,
        &frame,
        MouseEventKind::Drag(MouseButton::Left),
        end,
    );
    assert_eq!(
        copied(mouse(
            &mut state,
            &frame,
            MouseEventKind::Up(MouseButton::Left),
            end
        )),
        [uri]
    );
    assert_eq!(state.keyboard, owner);
}

#[test]
fn inactive_composer_can_be_copied_without_acquiring_keyboard_focus() {
    let mut state = reply("background");
    state.set_workspace_target(crate::state::WorkspaceTarget::SessionTitle);
    let owner = state.keyboard;
    let frame = draw(&mut state, 80, 24);
    let source = CopySource::Composer {
        session: state.selected,
        question: None,
    };
    let start = position(&frame, source, 1);
    let end = position(&frame, source, 5);
    mouse(
        &mut state,
        &frame,
        MouseEventKind::Down(MouseButton::Left),
        start,
    );
    assert_eq!(
        copied(mouse(
            &mut state,
            &frame,
            MouseEventKind::Up(MouseButton::Left),
            end
        )),
        ["b界"]
    );
    assert_eq!(state.keyboard, owner);
    assert_eq!(state.draft(), "ab界cd");
}

#[test]
fn failed_requests_open_complete_copyable_details_without_taking_keyboard_focus() {
    let message = format!(
        "routing 267 failed: model request failed (Provider): Invalid status code 400 Bad Request\n{}\n完整错误末尾",
        "provider detail: gpt-5.3 rejected\n".repeat(60)
    );
    for event in [
        SessionEvent::RoutingFailed {
            runtime: bone_app::RuntimeId::new(),
            inputs: vec![bone_app::InputId(1)],
            message: message.clone(),
        },
        SessionEvent::InputRejected {
            input: bone_app::InputId(1),
            message: message.clone(),
        },
    ] {
        for width in [60, 160] {
            let mut state = fixture(event.clone());
            let owner = state.keyboard;
            let frame = draw(&mut state, width, 40);
            let start = position(&frame, transcript(&state), 0);
            assert!(matches!(
                frame.hit(start.0, start.1),
                Some(ClickTarget::Action(Action::OpenHistory(SessionSeq(1))))
            ));
            mouse(
                &mut state,
                &frame,
                MouseEventKind::Down(MouseButton::Left),
                start,
            );
            assert!(state.details.is_none());
            mouse(
                &mut state,
                &frame,
                MouseEventKind::Up(MouseButton::Left),
                start,
            );
            assert!(
                state
                    .details
                    .as_ref()
                    .unwrap()
                    .content
                    .text
                    .starts_with(&message)
            );
            let frame = draw(&mut state, width, 40);
            let area = frame.layout.screen;
            let start = (area.y..area.bottom())
                .flat_map(|y| (area.x..area.right()).map(move |x| (x, y)))
                .find(|&(x, y)| {
                    frame.text_at(x, y).is_some_and(|point| {
                        matches!(point.source, CopySource::Details { .. }) && point.byte == 0
                    })
                })
                .unwrap();
            let source = frame.text_at(start.0, start.1).unwrap().source;
            let end = position(&frame, source, "routing".len());
            mouse(
                &mut state,
                &frame,
                MouseEventKind::Down(MouseButton::Left),
                start,
            );
            mouse(
                &mut state,
                &frame,
                MouseEventKind::Drag(MouseButton::Left),
                end,
            );
            assert_eq!(
                copied(mouse(
                    &mut state,
                    &frame,
                    MouseEventKind::Up(MouseButton::Left),
                    end
                )),
                ["routing"]
            );
            mouse(&mut state, &frame, MouseEventKind::ScrollDown, start);
            assert!(state.details.as_ref().unwrap().scroll > 0);
            assert_eq!(state.keyboard, owner);
            assert_eq!(state.draft(), "ab界cd");
        }
    }
}
