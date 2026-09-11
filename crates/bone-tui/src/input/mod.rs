//! Translation from terminal events to UI actions.
//!
//! This module is the only place that interprets terminal input. The keymap is
//! independent from pointer hit testing, while this facade supplies layout
//! measurements needed by both before emitting a `UiEvent`.

mod keymap;
mod pointer;

use crossterm::event::{Event, KeyCode, KeyEventKind};

use crate::{
    layout::LayoutMode,
    state::{Action, UiEvent, UiState},
    view::FrameSnapshot,
};

#[cfg(test)]
use keymap::STATUS_BASELINE_BINDINGS;
use keymap::key_action;
pub(crate) use keymap::{BindingHint, status_baseline_bindings};

pub(crate) fn terminal_event(
    event: Event,
    snapshot: Option<&FrameSnapshot>,
    state: &UiState,
) -> Option<UiEvent> {
    let action = match event {
        Event::Key(key)
            if key.kind == KeyEventKind::Press
                && key.code == KeyCode::Esc
                && state.dragging_divider.is_some() =>
        {
            Some(Action::EndPaneResize)
        }
        Event::Key(key) if key.kind == KeyEventKind::Press => key_action(key, state),
        Event::FocusLost => Some(Action::EndPaneResize),
        Event::Mouse(mouse) => pointer::action(mouse, snapshot, state),
        Event::Resize(width, height) => return Some(UiEvent::Resized { width, height }),
        Event::Paste(text) if matches!(state.panel, Some(crate::state::Panel::ModelSetup)) => {
            Some(Action::SetupText(text.into()))
        }
        Event::Paste(_) if state.panel.is_some() => None,
        Event::Paste(text) if state.focus == crate::state::Focus::SessionTitle => {
            Some(Action::TitlePaste(text))
        }
        Event::Paste(text) => Some(Action::Paste(text)),
        _ => None,
    }?;

    if snapshot.is_some_and(|frame| frame.layout.mode == LayoutMode::TooSmall)
        && !matches!(action, Action::Quit | Action::Terminate)
    {
        return None;
    }
    if matches!(action, Action::FocusRight)
        && matches!(
            state.focus,
            crate::state::Focus::SessionTitle | crate::state::Focus::Composer
        )
        && snapshot.is_none_or(|frame| frame.layout.extension_blank.is_none())
    {
        return None;
    }

    let action = match action {
        Action::ScrollPanel { amount, .. } => Action::ScrollPanel {
            amount,
            max: snapshot.map_or(0, |frame| frame.reader_max_scroll),
        },
        Action::MoveCursor {
            direction,
            select,
            word,
            ..
        } => Action::MoveCursor {
            direction,
            select,
            word,
            width: composer_width(snapshot),
        },
        Action::CursorVertical { down, .. } => Action::CursorVertical {
            down,
            width: composer_width(snapshot),
        },
        Action::ScrollUp {
            amount,
            metrics: None,
        } => Action::ScrollUp {
            amount,
            metrics: snapshot.and_then(|frame| frame.transcript_metrics.clone()),
        },
        action => action,
    };
    Some(UiEvent::Action(action))
}

fn composer_width(snapshot: Option<&FrameSnapshot>) -> u16 {
    snapshot
        .and_then(|frame| frame.layout.composer)
        .map_or(1, |area| crate::layout::composer_text_area(area).width)
}

#[cfg(test)]
use crate::{
    layout::{HitRegion, HitTarget, LayoutPlan},
    state::Focus,
    ui::interaction::HitMap,
};
#[cfg(test)]
use crossterm::event::{KeyEvent, KeyModifiers, MouseButton, MouseEventKind};

#[cfg(test)]
fn frame_for_layout(layout: LayoutPlan) -> FrameSnapshot {
    let mut hits = HitMap::default();
    if let Some(area) = layout.session_rail {
        hits.push(HitRegion {
            area,
            target: HitTarget::SessionRail,
        });
    }
    if let Some(area) = layout.transcript {
        hits.push(HitRegion {
            area,
            target: HitTarget::Conversation,
        });
    }
    if let Some(area) = layout.session_header {
        hits.push(HitRegion {
            area,
            target: HitTarget::SessionTitle,
        });
    }
    if let Some(area) = layout.composer {
        hits.push(HitRegion {
            area,
            target: HitTarget::Composer,
        });
    }
    if let Some(area) = layout.extension_blank {
        hits.push(HitRegion {
            area,
            target: HitTarget::RightRail,
        });
    }
    FrameSnapshot::new(layout, hits, None, 0)
}

#[cfg(test)]
fn mapped_key_action(key: KeyEvent, state: &UiState) -> Action {
    key_action(key, state).expect("key should map to an action")
}

#[cfg(test)]
fn mapped_terminal_event(
    event: Event,
    snapshot: Option<&FrameSnapshot>,
    state: &UiState,
) -> UiEvent {
    terminal_event(event, snapshot, state).expect("terminal event should map to a UI event")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn center_editors_page_history_and_keep_their_own_end_key() {
        let mut state = UiState::default();
        assert!(matches!(
            mapped_key_action(key(KeyCode::PageUp, KeyModifiers::NONE), &state),
            Action::ScrollUp { .. }
        ));
        assert!(matches!(
            mapped_key_action(key(KeyCode::PageDown, KeyModifiers::NONE), &state),
            Action::ScrollDown(10)
        ));
        assert!(matches!(
            mapped_key_action(key(KeyCode::End, KeyModifiers::NONE), &state),
            Action::CursorEnd
        ));
        state.focus = Focus::SessionTitle;
        assert!(matches!(
            mapped_key_action(key(KeyCode::PageUp, KeyModifiers::NONE), &state),
            Action::ScrollUp { .. }
        ));
        assert!(matches!(
            mapped_key_action(key(KeyCode::PageDown, KeyModifiers::NONE), &state),
            Action::ScrollDown(10)
        ));
        assert!(matches!(
            mapped_key_action(key(KeyCode::End, KeyModifiers::NONE), &state),
            Action::TitleEnd
        ));
    }

    #[test]
    fn ctrl_arrows_are_the_only_spatial_focus_keys() {
        for focus in [
            Focus::Sessions,
            Focus::SessionTitle,
            Focus::Composer,
            Focus::RightRail,
        ] {
            let mut state = UiState::default();
            state.focus = focus;
            assert!(matches!(
                mapped_key_action(key(KeyCode::Left, KeyModifiers::CONTROL), &state),
                Action::FocusLeft
            ));
            assert!(matches!(
                mapped_key_action(key(KeyCode::Right, KeyModifiers::CONTROL), &state),
                Action::FocusRight
            ));
            assert!(matches!(
                mapped_key_action(key(KeyCode::Up, KeyModifiers::CONTROL), &state),
                Action::FocusUp
            ));
            assert!(matches!(
                mapped_key_action(key(KeyCode::Down, KeyModifiers::CONTROL), &state),
                Action::FocusDown
            ));
            assert!(key_action(key(KeyCode::Tab, KeyModifiers::NONE), &state).is_none());
        }
    }
}

#[cfg(test)]
mod small_window_tests {
    use super::*;
    #[test]
    fn only_exit_and_resize_are_available_when_content_is_hidden() {
        let state = UiState::default();
        let plan = LayoutPlan::calculate(
            ratatui::layout::Rect::new(0, 0, 39, 12),
            crate::layout::SinglePane::Conversation,
            0,
            &[],
            None,
            1,
        );
        let snapshot = frame_for_layout(plan);
        assert!(
            terminal_event(
                Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
                Some(&snapshot),
                &state
            )
            .is_none()
        );
        assert!(matches!(
            mapped_terminal_event(
                Event::Key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)),
                Some(&snapshot),
                &state
            ),
            UiEvent::Action(Action::Quit)
        ));
        assert!(
            terminal_event(
                Event::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL)),
                Some(&snapshot),
                &state
            )
            .is_none()
        );
        assert!(matches!(
            mapped_terminal_event(Event::Resize(80, 24), Some(&snapshot), &state),
            UiEvent::Resized {
                width: 80,
                height: 24
            }
        ));
    }
}

#[cfg(test)]
mod model_keyboard_tests {
    use super::*;

    #[test]
    fn inline_title_editor_uses_title_specific_key_and_paste_actions() {
        let mut state = UiState::default();
        state.focus = Focus::SessionTitle;
        assert!(matches!(
            mapped_key_action(
                KeyEvent::new(KeyCode::Char('名'), KeyModifiers::NONE),
                &state
            ),
            Action::TitleInput('名')
        ));
        assert!(matches!(
            mapped_key_action(
                KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
                &state
            ),
            Action::TitleBackspace
        ));
        assert!(matches!(
            mapped_key_action(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &state),
            Action::CommitTitle
        ));
        assert!(
            matches!(mapped_terminal_event(Event::Paste("新标题".into()), None, &state), UiEvent::Action(Action::TitlePaste(text)) if text == "新标题")
        );
        assert!(matches!(
            mapped_key_action(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &state),
            Action::CancelTitle
        ));
    }

    #[test]
    fn command_chords_do_not_become_model_text() {
        let mut state = UiState::default();
        state.panel = Some(crate::state::Panel::Models);
        for modifiers in [KeyModifiers::CONTROL, KeyModifiers::ALT] {
            assert!(key_action(KeyEvent::new(KeyCode::Char('a'), modifiers), &state).is_none());
        }
        assert!(
            key_action(
                KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT),
                &state
            )
            .is_none()
        );
        assert!(matches!(
            mapped_key_action(
                KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
                &state
            ),
            Action::Quit
        ));
    }
    #[test]
    fn connection_form_keys_and_paste_use_secret_safe_actions() {
        let mut state = UiState::default();
        state.panel = Some(crate::state::Panel::ModelSetup);
        let event = mapped_terminal_event(Event::Paste("private-key-test".into()), None, &state);
        assert!(!format!("{event:?}").contains("private-key-test"));
        assert!(
            matches!(event, UiEvent::Action(Action::SetupText(text)) if text.as_str() == "private-key-test")
        );
        assert!(matches!(
            mapped_key_action(
                KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
                &state
            ),
            Action::SetupClear
        ));
        assert!(matches!(
            mapped_key_action(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &state),
            Action::NextField
        ));
    }

    #[test]
    fn clicking_visible_composer_moves_insertion_without_editing_draft() {
        let mut state = UiState::default();
        state.orphan_draft = "ab中文".into();
        state.orphan_cursor = state.orphan_draft.len();
        state.focus = Focus::SessionTitle;
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        let mut plan = None;
        terminal
            .draw(|frame| plan = Some(crate::view::render(frame, &state)))
            .unwrap();
        let plan = plan.unwrap();
        let text = crate::layout::composer_text_area(plan.composer.unwrap());
        let event = mapped_terminal_event(
            Event::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: text.x + 4,
                row: text.y,
                modifiers: KeyModifiers::NONE,
            }),
            Some(&plan),
            &state,
        );
        crate::state::update(&mut state, event);
        assert_eq!(state.orphan_cursor, 5);
        assert_eq!(state.orphan_draft, "ab中文");
        assert_eq!(state.orphan_revision, 0);
        assert_eq!(state.focus, Focus::Composer);
    }
}

#[cfg(test)]
mod session_navigation_tests {
    use super::*;
    #[test]
    fn session_keyboard_and_wheel_have_separate_actions() {
        let mut state = UiState::default();
        state.focus = Focus::Sessions;
        assert!(matches!(
            mapped_key_action(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &state),
            Action::SelectNext
        ));
        assert!(matches!(
            mapped_key_action(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &state),
            Action::OpenCandidate
        ));
        let plan = LayoutPlan::calculate(
            ratatui::layout::Rect::new(0, 0, 80, 20),
            crate::layout::SinglePane::Sessions,
            0,
            &[1; 30],
            Some(0),
            1,
        );
        let snapshot = frame_for_layout(plan);
        let event = Event::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 2,
            row: 4,
            modifiers: KeyModifiers::NONE,
        });
        assert!(matches!(
            mapped_terminal_event(event, Some(&snapshot), &state),
            UiEvent::Action(Action::ScrollSessions { start: 3 })
        ));
    }
}

#[cfg(test)]
mod selection_tests {
    use super::*;
    #[test]
    fn editor_shortcuts_and_drag_use_the_same_selected_buffer() {
        let mut state = UiState::default();
        for (key, expected) in [(KeyCode::Left, -1), (KeyCode::Down, 2), (KeyCode::Home, -3)] {
            assert!(
                matches!(mapped_key_action(KeyEvent::new(key, KeyModifiers::SHIFT), &state), Action::MoveCursor { direction, select: true, .. } if direction == expected)
            );
        }
        assert!(matches!(
            mapped_key_action(KeyEvent::new(KeyCode::Right, KeyModifiers::ALT), &state),
            Action::MoveCursor {
                word: true,
                select: false,
                ..
            }
        ));
        assert!(matches!(
            mapped_key_action(
                KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL),
                &state
            ),
            Action::Undo
        ));
        assert!(matches!(
            mapped_key_action(
                KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL),
                &state
            ),
            Action::Redo
        ));
        state.orphan_draft = "中e\u{301}🙂".into();
        state.orphan_cursor = state.orphan_draft.len();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        let mut plan = None;
        terminal
            .draw(|frame| plan = Some(crate::view::render(frame, &state)))
            .unwrap();
        let plan = plan.unwrap();
        let area = crate::layout::composer_text_area(plan.composer.unwrap());
        for (kind, x) in [
            (MouseEventKind::Down(MouseButton::Left), 2),
            (MouseEventKind::Drag(MouseButton::Left), 5),
        ] {
            let event = mapped_terminal_event(
                Event::Mouse(crossterm::event::MouseEvent {
                    kind,
                    column: area.x + x,
                    row: area.y,
                    modifiers: KeyModifiers::NONE,
                }),
                Some(&plan),
                &state,
            );
            crate::state::update(&mut state, event);
        }
        assert_eq!(
            state.editor().selection(state.draft_cursor()),
            Some(3..state.draft().len())
        );
        terminal
            .draw(|frame| {
                crate::view::render(frame, &state);
            })
            .unwrap();
        assert_ne!(
            terminal.backend().buffer()[(area.x + 2, area.y)].bg,
            terminal.backend().buffer()[(area.x, area.y)].bg
        );
        crate::state::update(&mut state, UiEvent::Action(Action::Backspace));
        assert_eq!(state.draft(), "中");
        crate::state::update(&mut state, UiEvent::Action(Action::Undo));
        assert_eq!(state.draft(), "中e\u{301}🙂");
    }

    #[test]
    fn title_shift_home_and_end_keep_the_editor_selection_anchor() {
        for focus in [Focus::SessionTitle, Focus::Composer] {
            let mut state = UiState::default();
            state.focus = focus;
            for (key, expected) in [(KeyCode::Home, -3), (KeyCode::End, 3)] {
                let action = mapped_key_action(KeyEvent::new(key, KeyModifiers::SHIFT), &state);
                assert!(match (focus, action) {
                    (
                        Focus::SessionTitle,
                        Action::TitleMoveCursor {
                            direction,
                            select: true,
                            word: false,
                        },
                    ) => direction == expected,
                    (
                        Focus::Composer,
                        Action::MoveCursor {
                            direction,
                            select: true,
                            word: false,
                            ..
                        },
                    ) => direction == expected,
                    _ => false,
                });
                for modifiers in [KeyModifiers::ALT, KeyModifiers::SHIFT | KeyModifiers::ALT] {
                    assert!(
                        key_action(KeyEvent::new(key, modifiers), &state).is_none(),
                        "{focus:?} must leave {modifiers:?}+{key:?} unbound"
                    );
                }
            }
        }
    }

    #[test]
    fn title_down_drag_and_shift_click_share_scrolled_single_line_geometry() {
        let title = format!("{}中e\u{301}🙂zTAIL", "0123456789".repeat(12));
        let info = bone_app::SessionInfo {
            id: bone_app::SessionId::new(),
            workspace: bone_app::WorkspaceId::new(),
            title: title.clone(),
            archived: false,
        };
        let mut state = UiState::default();
        state.sessions.push(info.clone());
        state.selected = Some(info.id);
        state
            .session_ui
            .insert(info.id, crate::state::SessionUi::new(info, 1));
        state.focus = Focus::SessionTitle;
        assert!(state.begin_title_edit());

        let draw = |state: &UiState| {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 24)).unwrap();
            let mut snapshot = None;
            terminal
                .draw(|frame| snapshot = Some(crate::view::render(frame, state)))
                .unwrap();
            snapshot.unwrap()
        };

        let snapshot = draw(&state);
        let header = snapshot.layout.session_header.unwrap();
        let origin = state.title_edit.as_ref().unwrap().editor.viewport_origin();
        let expected_down = crate::editor::cursor_at_single_line(&title, origin, 1);
        let down = mapped_terminal_event(
            Event::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: header.x + 1,
                row: header.y,
                modifiers: KeyModifiers::NONE,
            }),
            Some(&snapshot),
            &state,
        );
        assert!(
            matches!(&down, UiEvent::Action(Action::PlaceTitleCursor(byte)) if *byte == expected_down)
        );
        crate::state::update(&mut state, down);
        let edit = state.title_edit.as_ref().unwrap();
        assert_eq!(edit.cursor, expected_down);
        assert_eq!(edit.editor.selection(edit.cursor), None);

        let snapshot = draw(&state);
        let origin = state.title_edit.as_ref().unwrap().editor.viewport_origin();
        let expected_drag = crate::editor::cursor_at_single_line(&title, origin, 6);
        let drag = mapped_terminal_event(
            Event::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Drag(MouseButton::Left),
                column: header.x + 6,
                row: header.y,
                modifiers: KeyModifiers::NONE,
            }),
            Some(&snapshot),
            &state,
        );
        assert!(
            matches!(&drag, UiEvent::Action(Action::DragTitleCursor(byte)) if *byte == expected_drag)
        );
        crate::state::update(&mut state, drag);
        let edit = state.title_edit.as_ref().unwrap();
        assert_eq!(
            edit.editor.selection(edit.cursor),
            Some(expected_down.min(expected_drag)..expected_down.max(expected_drag))
        );

        let snapshot = draw(&state);
        let origin = state.title_edit.as_ref().unwrap().editor.viewport_origin();
        let expected_shift = crate::editor::cursor_at_single_line(&title, origin, 0);
        let shift_click = mapped_terminal_event(
            Event::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: header.x,
                row: header.y,
                modifiers: KeyModifiers::SHIFT,
            }),
            Some(&snapshot),
            &state,
        );
        assert!(
            matches!(&shift_click, UiEvent::Action(Action::DragTitleCursor(byte)) if *byte == expected_shift)
        );
        crate::state::update(&mut state, shift_click);
        let edit = state.title_edit.as_ref().unwrap();
        assert_eq!(
            edit.editor.selection(edit.cursor),
            Some(expected_down.min(expected_shift)..expected_down.max(expected_shift))
        );
    }
}

#[cfg(test)]
mod pointer_submit_tests {
    use super::*;
    #[test]
    fn visible_submit_click_is_distinct_from_reading_enter() {
        let mut state = UiState::default();
        state.orphan_draft = "send this".into();
        state.focus = Focus::RightRail;
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 40)).unwrap();
        let mut plan = None;
        terminal
            .draw(|frame| plan = Some(crate::view::render(frame, &state)))
            .unwrap();
        let plan = plan.unwrap();
        let submit = plan
            .hit_regions()
            .iter()
            .find(|region| region.target == HitTarget::Submit)
            .unwrap();
        assert!(matches!(
            mapped_terminal_event(
                Event::Mouse(crossterm::event::MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: submit.area.x,
                    row: submit.area.y,
                    modifiers: KeyModifiers::NONE
                }),
                Some(&plan),
                &state
            ),
            UiEvent::Action(Action::ClickSubmit)
        ));
        assert!(key_action(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &state).is_none());
    }
}

#[cfg(test)]
mod pane_resize_tests {
    use super::*;
    use crate::{layout::PaneDivider, state::update};
    use crossterm::event::MouseEvent;
    use ratatui::{Terminal, backend::TestBackend};

    fn draw(state: &UiState, width: u16) -> FrameSnapshot {
        let mut terminal = Terminal::new(TestBackend::new(width, 40)).unwrap();
        let mut plan = None;
        terminal
            .draw(|frame| plan = Some(crate::view::render(frame, state)))
            .unwrap();
        plan.unwrap()
    }

    fn mouse(
        state: &mut UiState,
        plan: &FrameSnapshot,
        kind: MouseEventKind,
        column: u16,
        row: u16,
    ) {
        if let Some(event) = terminal_event(
            Event::Mouse(MouseEvent {
                kind,
                column,
                row,
                modifiers: KeyModifiers::NONE,
            }),
            Some(plan),
            state,
        ) {
            assert!(
                update(state, event).is_empty(),
                "pane resizing must not issue product operations"
            );
        }
    }

    #[test]
    fn both_dividers_capture_drag_over_editor_and_release_without_editing() {
        for (divider, start, end) in [(PaneDivider::Left, 31, 51), (PaneDivider::Right, 120, 100)] {
            let mut state = UiState::default();
            state.orphan_draft = "草稿 e\u{301} stays unchanged".into();
            state.orphan_cursor = state.orphan_draft.len();
            let original = (state.orphan_draft.clone(), state.orphan_cursor, state.focus);
            let plan = draw(&state, 160);
            assert_eq!(plan.hit(start, 0), Some(HitTarget::PaneDivider(divider)));
            mouse(
                &mut state,
                &plan,
                MouseEventKind::Down(MouseButton::Left),
                start,
                0,
            );
            mouse(
                &mut state,
                &plan,
                MouseEventKind::Drag(MouseButton::Left),
                end,
                plan.composer.unwrap().y + 1,
            );
            assert_eq!(state.dragging_divider, Some(divider));
            let resized = draw(&state, 160);
            match divider {
                PaneDivider::Left => assert_eq!(resized.session_rail.unwrap().width, 52),
                PaneDivider::Right => assert_eq!(resized.extension_blank.unwrap().width, 60),
            }
            assert_eq!(resized.conversation.unwrap().width, 68);
            assert_eq!(resized.composer.unwrap().width, 60);
            mouse(
                &mut state,
                &resized,
                MouseEventKind::Up(MouseButton::Left),
                end,
                39,
            );
            assert_eq!(state.dragging_divider, None);
            let saved = state.pane_widths;
            mouse(
                &mut state,
                &resized,
                MouseEventKind::Drag(MouseButton::Left),
                0,
                0,
            );
            assert_eq!(state.pane_widths, saved);
            assert_eq!(
                (state.orphan_draft.clone(), state.orphan_cursor, state.focus),
                original
            );
        }
    }

    #[test]
    fn dragging_clamps_and_window_resize_retains_preferences_but_releases_capture() {
        let mut state = UiState::default();
        let plan = draw(&state, 160);
        mouse(
            &mut state,
            &plan,
            MouseEventKind::Down(MouseButton::Left),
            31,
            0,
        );
        mouse(
            &mut state,
            &plan,
            MouseEventKind::Drag(MouseButton::Left),
            u16::MAX,
            0,
        );
        assert_eq!(state.pane_widths.left, 64);
        assert_eq!(draw(&state, 160).conversation.unwrap().width, 56);
        state.focus = Focus::RightRail;
        update(
            &mut state,
            UiEvent::Resized {
                width: 80,
                height: 40,
            },
        );
        assert_eq!(state.dragging_divider, None);
        assert_eq!(state.focus, Focus::Composer);
        let narrow = draw(&state, 80);
        assert!(
            !narrow
                .hit_regions()
                .iter()
                .any(|hit| matches!(hit.target, HitTarget::PaneDivider(_)))
        );
        assert_eq!(draw(&state, 120).session_rail.unwrap().width, 64);
        assert_eq!(draw(&state, 100).session_rail.unwrap().width, 44);
        assert_eq!(draw(&state, 160).session_rail.unwrap().width, 64);
        // Explicit dragging after a window clamp keeps the opposite visible edge stationary.
        let small = draw(&state, 140);
        let right = small.extension_blank.unwrap().width;
        let edge = small.session_rail.unwrap().right() - 1;
        mouse(
            &mut state,
            &small,
            MouseEventKind::Down(MouseButton::Left),
            edge,
            0,
        );
        mouse(
            &mut state,
            &small,
            MouseEventKind::Up(MouseButton::Left),
            39,
            0,
        );
        assert_eq!(draw(&state, 140).extension_blank.unwrap().width, right);
        let plan = draw(&state, 160);
        mouse(
            &mut state,
            &plan,
            MouseEventKind::Down(MouseButton::Left),
            plan.extension_blank.unwrap().x,
            0,
        );
        mouse(
            &mut state,
            &plan,
            MouseEventKind::Up(MouseButton::Left),
            u16::MAX,
            0,
        );
        assert_eq!(state.pane_widths.right, 24);
        assert_eq!(state.dragging_divider, None);
    }

    #[test]
    fn an_open_panel_owns_pointer_input_and_escape_closes_that_scope() {
        let mut state = UiState::default();
        update(&mut state, UiEvent::Action(Action::OpenHelp));
        let plan = draw(&state, 160);
        assert!(
            !plan
                .hit_regions()
                .iter()
                .any(|hit| matches!(hit.target, HitTarget::PaneDivider(_)))
        );
        mouse(
            &mut state,
            &plan,
            MouseEventKind::Down(MouseButton::Left),
            31,
            0,
        );
        assert_eq!(state.dragging_divider, None);
        let event = mapped_terminal_event(
            Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            Some(&plan),
            &state,
        );
        update(&mut state, event);
        assert_eq!(state.dragging_divider, None);
        assert!(state.panel.is_none());
    }
}

#[cfg(test)]
mod input_chord_tests {
    use super::*;
    use crate::state::{Panel, update};

    #[test]
    fn clear_is_undoable_empty_clear_is_inert_and_panels_protect_the_draft() {
        let mut state = UiState::default();
        state.orphan_draft = "中文 e\u{301}\nkeep me".into();
        state.orphan_cursor = state.orphan_draft.len();
        let original = state.orphan_draft.clone();
        let clear = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        let action = mapped_key_action(clear, &state);
        assert!(matches!(action, Action::ClearInput));
        assert!(update(&mut state, UiEvent::Action(action)).is_empty());
        assert!(state.draft().is_empty());
        assert!(!state.quitting);
        let revision = state.orphan_revision;
        update(&mut state, UiEvent::Action(Action::ClearInput));
        assert_eq!(state.orphan_revision, revision);
        update(&mut state, UiEvent::Action(Action::Undo));
        assert_eq!(state.draft(), original);
        assert!(state.orphan_revision > revision);
        for focus in [Focus::Sessions, Focus::SessionTitle, Focus::RightRail] {
            state.focus = focus;
            assert!(key_action(clear, &state).is_none());
        }
        state.focus = Focus::Composer;
        for panel in [Panel::Models, Panel::Help, Panel::ModelSetup] {
            state.panel = Some(panel);
            assert!(key_action(clear, &state).is_none());
            assert!(matches!(
                mapped_key_action(
                    KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
                    &state
                ),
                Action::Quit
            ));
        }
        assert_eq!(state.draft(), original);
    }

    #[test]
    fn composer_submission_and_newline_use_only_the_documented_chords() {
        let state = UiState::default();
        assert!(matches!(
            mapped_key_action(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &state),
            Action::Submit
        ));
        assert!(matches!(
            mapped_key_action(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT), &state),
            Action::InsertNewline
        ));
        assert!(key_action(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT), &state).is_none());
        assert!(
            key_action(
                KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL),
                &state
            )
            .is_none()
        );
        assert!(
            key_action(
                KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL),
                &state
            )
            .is_none()
        );
        assert!(
            key_action(
                KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
                &state
            )
            .is_none()
        );
        assert!(matches!(
            mapped_key_action(
                KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
                &state
            ),
            Action::Quit
        ));
    }

    #[test]
    fn modified_enter_never_leaks_into_other_focuses_or_panels() {
        let mut state = UiState::default();
        for focus in [Focus::Sessions, Focus::SessionTitle, Focus::RightRail] {
            state.focus = focus;
            assert!(
                key_action(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT), &state).is_none()
            );
        }
        state.focus = Focus::Composer;
        for panel in [Panel::Models, Panel::Help, Panel::ModelSetup] {
            state.panel = Some(panel);
            for modifiers in [
                KeyModifiers::SHIFT,
                KeyModifiers::ALT,
                KeyModifiers::CONTROL,
            ] {
                assert!(key_action(KeyEvent::new(KeyCode::Enter, modifiers), &state).is_none());
            }
        }
    }

    #[test]
    fn ctrl_d_is_global_and_ctrl_p_and_ctrl_q_remain_unbound() {
        let mut state = UiState::default();
        for focus in [
            Focus::Composer,
            Focus::Sessions,
            Focus::SessionTitle,
            Focus::RightRail,
        ] {
            state.focus = focus;
            state.panel = None;
            assert!(matches!(
                mapped_key_action(
                    KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
                    &state
                ),
                Action::Quit
            ));
            assert!(
                key_action(
                    KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL),
                    &state
                )
                .is_none()
            );
            assert!(
                key_action(
                    KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
                    &state
                )
                .is_none()
            );
        }
        for panel in [Panel::Models, Panel::Help, Panel::ModelSetup] {
            state.panel = Some(panel);
            assert!(matches!(
                mapped_key_action(
                    KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
                    &state
                ),
                Action::Quit
            ));
            assert!(
                key_action(
                    KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL),
                    &state
                )
                .is_none()
            );
            assert!(
                key_action(
                    KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
                    &state
                )
                .is_none()
            );
        }
    }

    #[test]
    fn visible_binding_hints_describe_the_active_contract() {
        assert_eq!(
            STATUS_BASELINE_BINDINGS,
            [
                BindingHint {
                    chord: "shift+enter",
                    label: "newline"
                },
                BindingHint {
                    chord: "ctrl+c",
                    label: "clear"
                },
                BindingHint {
                    chord: "ctrl+d",
                    label: "exit"
                },
            ]
        );
    }
}
