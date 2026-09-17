//! Translation from terminal events to UI actions.
//!
//! Keyboard ownership and pointer geometry are independent input routes.

pub(crate) mod commands;
mod keymap;
mod pointer;

use crossterm::event::{Event, KeyCode, KeyEventKind};

use crate::{
    editor::EditCommand,
    layout::LayoutMode,
    state::{Action, EditorTarget, KeyboardOwner, UiEvent, UiState, WorkspaceTarget},
    view::FrameSnapshot,
};

#[cfg(test)]
use keymap::STATUS_BASELINE_BINDINGS;
pub(crate) use keymap::{BindingHint, status_baseline_bindings};
use keymap::{KeyGeometry, key_action};

pub(crate) fn terminal_event(
    event: Event,
    snapshot: Option<&FrameSnapshot>,
    state: &UiState,
) -> Option<UiEvent> {
    let key_geometry = KeyGeometry {
        composer_width: snapshot
            .and_then(|frame| frame.layout.composer)
            .map(crate::layout::composer_text_area)
            .map(|area| area.width),
    };
    let action = match event {
        Event::Key(key)
            if key.kind == KeyEventKind::Press
                && key.code == KeyCode::Esc
                && state.pointer.capture.is_some() =>
        {
            return Some(UiEvent::CancelPointerCapture);
        }
        Event::Key(key) if key.kind == KeyEventKind::Press => key_action(key, state, key_geometry),
        Event::FocusLost => return Some(UiEvent::PointerLeft),
        Event::Mouse(mouse) => pointer::action(mouse, snapshot, state),
        Event::Resize(_, _) => return Some(UiEvent::Resized),
        Event::Paste(text) => match state.keyboard {
            KeyboardOwner::Overlay { .. }
                if matches!(
                    &state.overlay,
                    Some(crate::state::Overlay::Models(models))
                        if matches!(
                            &models.screen,
                            crate::state::ModelScreen::Setup(_)
                                | crate::state::ModelScreen::ModelForm(_)
                        )
                ) =>
            {
                Some(Action::SetupText(text.into()))
            }
            KeyboardOwner::Overlay { .. } => None,
            KeyboardOwner::Workspace(target) => {
                let target = match target {
                    WorkspaceTarget::SessionTitle => EditorTarget::SessionTitle,
                    WorkspaceTarget::Composer => EditorTarget::Composer,
                    WorkspaceTarget::Sessions => return None,
                };
                Some(Action::Edit {
                    target,
                    command: EditCommand::Insert {
                        text,
                        typing: false,
                    },
                })
            }
        },
        _ => None,
    }?;

    if snapshot.is_some_and(|frame| frame.layout.mode == LayoutMode::TooSmall)
        && !matches!(
            action,
            Action::Quit | Action::EndPointerCapture | Action::ReleasePointer { .. }
        )
    {
        return None;
    }
    Some(UiEvent::Action(action))
}

#[cfg(test)]
use crate::{
    editor::CursorMove,
    layout::{ClickRegion, ClickTarget, LayoutPlan},
    state::PointerCapture,
    ui::interaction::{FrameHits, ScrollTarget, SurfaceHits},
};
#[cfg(test)]
use crossterm::event::{KeyEvent, KeyModifiers, MouseButton, MouseEventKind};

#[cfg(test)]
fn frame_for_layout(layout: LayoutPlan) -> FrameSnapshot {
    let mut hits = SurfaceHits::default();
    if let Some(area) = layout.session_rail {
        hits.push_scroll(area, ScrollTarget::Sessions);
    }
    if let Some(area) = layout.transcript {
        hits.push_scroll(area, ScrollTarget::Conversation);
    }
    if let Some(area) = layout.session_header {
        hits.push(ClickRegion {
            area,
            target: ClickTarget::Editor(EditorTarget::SessionTitle),
        });
    }
    if let Some(area) = layout.composer {
        hits.push(ClickRegion {
            area,
            target: ClickTarget::Editor(EditorTarget::Composer),
        });
    }
    FrameSnapshot::new(
        layout,
        FrameHits::new(hits, None),
        None,
        0,
        Some(0),
        Some(0),
    )
}

#[cfg(test)]
fn model_panel(screen: crate::state::ModelScreen) -> crate::state::Overlay {
    let mut models = crate::state::ModelPanel::new(None);
    models.screen = screen;
    crate::state::Overlay::Models(models)
}

#[cfg(test)]
fn model_list_panel() -> crate::state::Overlay {
    model_panel(crate::state::ModelScreen::List { selected: 0 })
}

#[cfg(test)]
fn model_setup_panel() -> crate::state::Overlay {
    model_panel(crate::state::ModelScreen::Setup(Box::new(
        crate::state::ConnectionForm::new(crate::state::ConnectionKind::OpenAiApi),
    )))
}

#[cfg(test)]
fn composer_edit(command: EditCommand) -> Action {
    Action::Edit {
        target: EditorTarget::Composer,
        command,
    }
}

#[cfg(test)]
fn test_key_action(key: KeyEvent, state: &UiState) -> Option<Action> {
    key_action(
        key,
        state,
        KeyGeometry {
            composer_width: Some(37),
        },
    )
}

#[cfg(test)]
fn mapped_key_action(key: KeyEvent, state: &UiState) -> Action {
    test_key_action(key, state).expect("key should map to an action")
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
            Action::ScrollUp(10)
        ));
        assert!(matches!(
            mapped_key_action(key(KeyCode::PageDown, KeyModifiers::NONE), &state),
            Action::ScrollDown(10)
        ));
        assert!(matches!(
            mapped_key_action(key(KeyCode::End, KeyModifiers::NONE), &state),
            Action::Edit {
                target: EditorTarget::Composer,
                command: EditCommand::Move {
                    cursor: CursorMove::LineEnd,
                    select: false,
                }
            }
        ));
        state.set_workspace_target(WorkspaceTarget::SessionTitle);
        assert!(matches!(
            mapped_key_action(key(KeyCode::PageUp, KeyModifiers::NONE), &state),
            Action::ScrollUp(10)
        ));
        assert!(matches!(
            mapped_key_action(key(KeyCode::PageDown, KeyModifiers::NONE), &state),
            Action::ScrollDown(10)
        ));
        assert!(matches!(
            mapped_key_action(key(KeyCode::End, KeyModifiers::NONE), &state),
            Action::Edit {
                target: EditorTarget::SessionTitle,
                command: EditCommand::Move {
                    cursor: CursorMove::LineEnd,
                    select: false,
                }
            }
        ));
    }

    #[test]
    fn ctrl_arrows_are_the_only_spatial_focus_keys() {
        for focus in [
            WorkspaceTarget::Sessions,
            WorkspaceTarget::SessionTitle,
            WorkspaceTarget::Composer,
        ] {
            let mut state = UiState::default();
            state.set_workspace_target(focus);
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
            assert!(test_key_action(key(KeyCode::Tab, KeyModifiers::NONE), &state).is_none());
        }
    }

    #[test]
    fn terminal_keys_use_the_completed_frame_geometry() {
        let mut state = UiState::default();
        let layout = LayoutPlan::calculate_with_widths(
            ratatui::layout::Rect::new(0, 0, 100, 30),
            crate::layout::SinglePane::Conversation,
            0,
            None,
            None,
            1,
            crate::layout::PaneWidths::default(),
        );
        assert!(layout.extension_blank.is_none());
        let width = crate::layout::composer_text_area(layout.composer.unwrap()).width;
        let snapshot = frame_for_layout(layout);

        assert!(matches!(
            mapped_terminal_event(
                Event::Key(key(KeyCode::Down, KeyModifiers::NONE)),
                Some(&snapshot),
                &state,
            ),
            UiEvent::Action(Action::Edit {
                target: EditorTarget::Composer,
                command: EditCommand::Move {
                    cursor: CursorMove::Down { width: actual },
                    select: false,
                },
            }) if actual == width
        ));
        assert!(
            terminal_event(
                Event::Key(key(KeyCode::Down, KeyModifiers::NONE)),
                None,
                &state,
            )
            .is_none()
        );
        assert!(
            terminal_event(
                Event::Key(key(KeyCode::Right, KeyModifiers::CONTROL)),
                Some(&snapshot),
                &state,
            )
            .is_some()
        );

        let wide = frame_for_layout(LayoutPlan::calculate_with_widths(
            ratatui::layout::Rect::new(0, 0, 160, 30),
            crate::layout::SinglePane::Conversation,
            0,
            None,
            None,
            1,
            crate::layout::PaneWidths::default(),
        ));
        assert!(matches!(
            mapped_terminal_event(
                Event::Key(key(KeyCode::Right, KeyModifiers::CONTROL)),
                Some(&wide),
                &state,
            ),
            UiEvent::Action(Action::FocusRight)
        ));

        state.set_workspace_target(WorkspaceTarget::Sessions);
        assert!(matches!(
            mapped_terminal_event(
                Event::Key(key(KeyCode::Right, KeyModifiers::CONTROL)),
                Some(&snapshot),
                &state,
            ),
            UiEvent::Action(Action::FocusRight)
        ));
    }

    #[test]
    fn reader_wheel_uses_the_completed_frame_scroll_limit() {
        let layout = LayoutPlan::calculate_with_widths(
            ratatui::layout::Rect::new(0, 0, 100, 30),
            crate::layout::SinglePane::Conversation,
            0,
            None,
            None,
            1,
            crate::layout::PaneWidths::default(),
        );
        let area = layout.transcript.unwrap();
        let mut hits = SurfaceHits::default();
        hits.push_scroll(area, ScrollTarget::Details);
        let snapshot = FrameSnapshot::new(layout, FrameHits::new(hits, None), None, 47, None, None);

        assert!(matches!(
            mapped_terminal_event(
                Event::Mouse(crossterm::event::MouseEvent {
                    kind: MouseEventKind::ScrollDown,
                    column: area.x,
                    row: area.y,
                    modifiers: KeyModifiers::NONE,
                }),
                Some(&snapshot),
                &UiState::default(),
            ),
            UiEvent::Action(Action::ScrollDetails { amount: 3, max: 47 })
        ));
    }
}

#[cfg(test)]
mod small_window_tests {
    use super::*;
    #[test]
    fn only_exit_and_resize_are_available_when_content_is_hidden() {
        let state = UiState::default();
        let plan = LayoutPlan::calculate_with_widths(
            ratatui::layout::Rect::new(0, 0, 39, 12),
            crate::layout::SinglePane::Conversation,
            0,
            None,
            None,
            1,
            crate::layout::PaneWidths::default(),
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
            UiEvent::Resized
        ));
    }
}

#[cfg(test)]
mod model_keyboard_tests {
    use super::*;

    #[test]
    fn inline_title_editor_uses_title_specific_key_and_paste_actions() {
        let mut state = UiState::default();
        state.set_workspace_target(WorkspaceTarget::SessionTitle);
        assert!(matches!(
            mapped_key_action(
                KeyEvent::new(KeyCode::Char('名'), KeyModifiers::NONE),
                &state
            ),
            Action::Edit {
                target: EditorTarget::SessionTitle,
                command: EditCommand::Insert { ref text, typing: true }
            } if text == "名"
        ));
        assert!(matches!(
            mapped_key_action(
                KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
                &state
            ),
            Action::Edit {
                target: EditorTarget::SessionTitle,
                command: EditCommand::DeleteBefore
            }
        ));
        assert!(matches!(
            mapped_key_action(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &state),
            Action::CommitTitle
        ));
        assert!(
            matches!(mapped_terminal_event(Event::Paste("新标题".into()), None, &state), UiEvent::Action(Action::Edit { target: EditorTarget::SessionTitle, command: EditCommand::Insert { text, typing: false } }) if text == "新标题")
        );
        assert!(matches!(
            mapped_key_action(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &state),
            Action::CancelTitle
        ));
    }

    #[test]
    fn command_chords_do_not_become_model_text() {
        let mut state = UiState::default();
        state.overlay = Some(model_list_panel());
        state.enter_overlay();
        for modifiers in [KeyModifiers::CONTROL, KeyModifiers::ALT] {
            assert!(
                test_key_action(KeyEvent::new(KeyCode::Char('a'), modifiers), &state).is_none()
            );
        }
        assert!(
            test_key_action(
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
        state.overlay = Some(model_setup_panel());
        state.enter_overlay();
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
        state.set_workspace_target(WorkspaceTarget::Composer);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        let mut plan = None;
        terminal
            .draw(|frame| plan = Some(crate::view::render(frame, &state)))
            .unwrap();
        let plan = plan.unwrap();
        let text = crate::layout::composer_text_area(plan.layout.composer.unwrap());
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
        assert_eq!(state.draft_cursor(), 5);
        assert_eq!(state.draft(), "ab中文");
        assert_eq!(state.orphan_draft.revision(), 0);
        assert_eq!(state.workspace_target(), WorkspaceTarget::Composer);
    }
}

#[cfg(test)]
mod session_navigation_tests {
    use super::*;
    #[test]
    fn session_keyboard_and_wheel_have_separate_actions() {
        let mut state = UiState::default();
        state.set_workspace_target(WorkspaceTarget::Sessions);
        assert!(matches!(
            mapped_key_action(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &state),
            Action::SelectNext
        ));
        assert!(matches!(
            mapped_key_action(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &state),
            Action::OpenCandidate
        ));
        let plan = LayoutPlan::calculate_with_widths(
            ratatui::layout::Rect::new(0, 0, 80, 20),
            crate::layout::SinglePane::Sessions,
            30,
            Some(0),
            None,
            1,
            crate::layout::PaneWidths::default(),
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
    fn scrolled_composer_clicks_use_the_rendered_buffer_origin() {
        let mut state = UiState::default();
        state.orphan_draft = (0..20)
            .map(|row| format!("row {row}"))
            .collect::<Vec<_>>()
            .join("\n")
            .into();
        let mut view_state = crate::ui::frame::ViewState::default();
        let draw = |state: &UiState, view_state: &mut crate::ui::frame::ViewState| {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 12)).unwrap();
            let mut snapshot = None;
            terminal
                .draw(|frame| {
                    snapshot = Some(crate::view::render_with_view_state(
                        frame, state, view_state,
                    ));
                })
                .unwrap();
            snapshot.unwrap()
        };

        let snapshot = draw(&state, &mut view_state);
        let origin = snapshot.composer_row_origin().unwrap();
        assert!(origin > 0);
        let area = crate::layout::composer_text_area(snapshot.layout.composer.unwrap());
        let expected = crate::editor::cursor_at_origin(state.draft(), area.width, origin, 2, 0);
        let click = mapped_terminal_event(
            Event::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: area.x + 2,
                row: area.y,
                modifiers: KeyModifiers::NONE,
            }),
            Some(&snapshot),
            &state,
        );
        assert!(matches!(
            &click,
            UiEvent::Action(Action::PointEditor {
                target: EditorTarget::Composer,
                byte, extend: false, begin: true,
            }) if *byte == expected
        ));
        crate::state::update(&mut state, click);
        assert_eq!(state.draft_cursor(), expected);

        let snapshot = draw(&state, &mut view_state);
        assert_eq!(snapshot.composer_row_origin(), Some(origin));
        let expected_end = crate::editor::cursor_at_origin(state.draft(), area.width, origin, 4, 1);
        let shift_click = mapped_terminal_event(
            Event::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: area.x + 4,
                row: area.y + 1,
                modifiers: KeyModifiers::SHIFT,
            }),
            Some(&snapshot),
            &state,
        );
        crate::state::update(&mut state, shift_click);
        assert_eq!(
            state.editor().selection(),
            Some(expected.min(expected_end)..expected.max(expected_end))
        );
    }

    #[test]
    fn editor_shortcuts_and_drag_use_the_same_selected_buffer() {
        let mut state = UiState::default();
        for (key, expected) in [
            (KeyCode::Left, CursorMove::Left),
            (KeyCode::Down, CursorMove::Down { width: 37 }),
            (KeyCode::Home, CursorMove::LineStart),
        ] {
            assert!(
                matches!(mapped_key_action(KeyEvent::new(key, KeyModifiers::SHIFT), &state), Action::Edit { target: EditorTarget::Composer, command: EditCommand::Move { cursor, select: true } } if cursor == expected)
            );
        }
        assert!(matches!(
            mapped_key_action(KeyEvent::new(KeyCode::Right, KeyModifiers::ALT), &state),
            Action::Edit {
                target: EditorTarget::Composer,
                command: EditCommand::Move {
                    cursor: CursorMove::WordRight,
                    select: false,
                }
            }
        ));
        assert!(matches!(
            mapped_key_action(
                KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL),
                &state
            ),
            Action::Edit {
                target: EditorTarget::Composer,
                command: EditCommand::Undo
            }
        ));
        assert!(matches!(
            mapped_key_action(
                KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL),
                &state
            ),
            Action::Edit {
                target: EditorTarget::Composer,
                command: EditCommand::Redo
            }
        ));
        state.orphan_draft = "中e\u{301}🙂".into();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        let mut plan = None;
        terminal
            .draw(|frame| plan = Some(crate::view::render(frame, &state)))
            .unwrap();
        let plan = plan.unwrap();
        let area = crate::layout::composer_text_area(plan.layout.composer.unwrap());
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
        assert_eq!(state.editor().selection(), Some(3..state.draft().len()));
        terminal
            .draw(|frame| {
                crate::view::render(frame, &state);
            })
            .unwrap();
        assert_ne!(
            terminal.backend().buffer()[(area.x + 2, area.y)].bg,
            terminal.backend().buffer()[(area.x, area.y)].bg
        );
        crate::state::update(
            &mut state,
            UiEvent::Action(composer_edit(EditCommand::DeleteBefore)),
        );
        assert_eq!(state.draft(), "中");
        crate::state::update(
            &mut state,
            UiEvent::Action(composer_edit(EditCommand::Undo)),
        );
        assert_eq!(state.draft(), "中e\u{301}🙂");
    }

    #[test]
    fn title_shift_home_and_end_keep_the_editor_selection_anchor() {
        for focus in [WorkspaceTarget::SessionTitle, WorkspaceTarget::Composer] {
            let mut state = UiState::default();
            state.set_workspace_target(focus);
            for (key, expected) in [
                (KeyCode::Home, CursorMove::LineStart),
                (KeyCode::End, CursorMove::LineEnd),
            ] {
                let action = mapped_key_action(KeyEvent::new(key, KeyModifiers::SHIFT), &state);
                let target = if focus == WorkspaceTarget::SessionTitle {
                    EditorTarget::SessionTitle
                } else {
                    EditorTarget::Composer
                };
                assert!(
                    matches!(action, Action::Edit { target: actual, command: EditCommand::Move { cursor, select: true } } if actual == target && cursor == expected)
                );
                for modifiers in [KeyModifiers::ALT, KeyModifiers::SHIFT | KeyModifiers::ALT] {
                    assert!(
                        test_key_action(KeyEvent::new(key, modifiers), &state).is_none(),
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
        state
            .session_rows
            .push(crate::state::SessionNavRow::provisional(info.clone()));
        state.selected = Some(info.id);
        state
            .session_ui
            .insert(info.id, crate::state::SessionUi::new(info.id, 1));
        state.set_workspace_target(WorkspaceTarget::SessionTitle);
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
        let origin = snapshot.title_byte_origin().unwrap();
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
            matches!(&down, UiEvent::Action(Action::PointEditor { target: EditorTarget::SessionTitle, byte, extend: false, begin: true }) if *byte == expected_down)
        );
        crate::state::update(&mut state, down);
        let editor = state.title_editor().unwrap();
        assert_eq!(editor.cursor(), expected_down);
        assert_eq!(editor.selection(), None);

        let snapshot = draw(&state);
        let origin = snapshot.title_byte_origin().unwrap();
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
            matches!(&drag, UiEvent::Action(Action::PointEditor { target: EditorTarget::SessionTitle, byte, extend: true, .. }) if *byte == expected_drag)
        );
        crate::state::update(&mut state, drag);
        let editor = state.title_editor().unwrap();
        assert_eq!(
            editor.selection(),
            Some(expected_down.min(expected_drag)..expected_down.max(expected_drag))
        );

        let snapshot = draw(&state);
        let origin = snapshot.title_byte_origin().unwrap();
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
            matches!(&shift_click, UiEvent::Action(Action::PointEditor { target: EditorTarget::SessionTitle, byte, extend: true, .. }) if *byte == expected_shift)
        );
        crate::state::update(&mut state, shift_click);
        let editor = state.title_editor().unwrap();
        assert_eq!(
            editor.selection(),
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
        state.set_workspace_target(WorkspaceTarget::SessionTitle);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 40)).unwrap();
        let mut plan = None;
        terminal
            .draw(|frame| plan = Some(crate::view::render(frame, &state)))
            .unwrap();
        let plan = plan.unwrap();
        let submit = plan
            .hit_regions()
            .into_iter()
            .find(|region| region.target == ClickTarget::Action(Action::Submit))
            .unwrap();
        let press = mapped_terminal_event(
            Event::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: submit.area.x,
                row: submit.area.y,
                modifiers: KeyModifiers::NONE,
            }),
            Some(&plan),
            &state,
        );
        assert!(crate::state::update(&mut state, press).is_empty());
        assert_eq!(state.draft(), "send this");
        let release = mapped_terminal_event(
            Event::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Up(MouseButton::Left),
                column: submit.area.x,
                row: submit.area.y,
                modifiers: KeyModifiers::NONE,
            }),
            Some(&plan),
            &state,
        );
        assert!(
            matches!(&release, UiEvent::Action(Action::ReleasePointer { click: Some(action), .. }) if **action == Action::Submit)
        );
        let effects = crate::state::update(&mut state, release);
        assert!(
            effects
                .iter()
                .any(|effect| matches!(effect, crate::state::Effect::CreateSession { .. }))
        );
        assert!(matches!(
            test_key_action(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &state),
            Some(Action::CommitTitle)
        ));
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
    fn focus_loss_only_releases_an_active_divider_drag() {
        let mut state = UiState::default();
        assert!(matches!(
            terminal_event(Event::FocusLost, None, &state),
            Some(UiEvent::PointerLeft)
        ));

        state.pointer.capture = Some(PointerCapture::Divider(PaneDivider::Left));
        assert!(matches!(
            terminal_event(Event::FocusLost, None, &state),
            Some(UiEvent::PointerLeft)
        ));
    }

    #[test]
    fn both_dividers_capture_drag_over_editor_and_release_without_editing() {
        for (divider, start, end) in [(PaneDivider::Left, 31, 51), (PaneDivider::Right, 120, 100)] {
            let mut state = UiState::default();
            state.orphan_draft = "草稿 e\u{301} stays unchanged".into();
            let original = (
                state.orphan_draft.clone(),
                state.draft_cursor(),
                state.workspace_target(),
            );
            let plan = draw(&state, 160);
            assert_eq!(plan.hit(start, 0), Some(ClickTarget::PaneDivider(divider)));
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
                plan.layout.composer.unwrap().y + 1,
            );
            assert_eq!(
                state.pointer.capture,
                Some(PointerCapture::Divider(divider))
            );
            let resized = draw(&state, 160);
            match divider {
                PaneDivider::Left => assert_eq!(resized.layout.session_rail.unwrap().width, 52),
                PaneDivider::Right => {
                    assert_eq!(resized.layout.extension_blank.unwrap().width, 60)
                }
            }
            assert_eq!(resized.layout.conversation.unwrap().width, 68);
            assert_eq!(resized.layout.composer.unwrap().width, 60);
            mouse(
                &mut state,
                &resized,
                MouseEventKind::Up(MouseButton::Left),
                end,
                39,
            );
            assert_eq!(state.pointer.capture, None);
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
                (
                    state.orphan_draft.clone(),
                    state.draft_cursor(),
                    state.workspace_target(),
                ),
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
        assert_eq!(draw(&state, 160).layout.conversation.unwrap().width, 56);
        state.set_workspace_target(WorkspaceTarget::Composer);
        update(&mut state, UiEvent::Resized);
        assert_eq!(state.pointer.capture, None);
        assert_eq!(state.workspace_target(), WorkspaceTarget::Composer);
        let narrow = draw(&state, 80);
        assert!(
            !narrow
                .hit_regions()
                .iter()
                .any(|hit| matches!(hit.target, ClickTarget::PaneDivider(_)))
        );
        assert_eq!(draw(&state, 120).layout.session_rail.unwrap().width, 64);
        assert_eq!(draw(&state, 100).layout.session_rail.unwrap().width, 44);
        assert_eq!(draw(&state, 160).layout.session_rail.unwrap().width, 64);
        // Explicit dragging after a window clamp keeps the opposite visible edge stationary.
        let small = draw(&state, 140);
        let right = small.layout.extension_blank.unwrap().width;
        let edge = small.layout.session_rail.unwrap().right() - 1;
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
        assert_eq!(
            draw(&state, 140).layout.extension_blank.unwrap().width,
            right
        );
        let plan = draw(&state, 160);
        mouse(
            &mut state,
            &plan,
            MouseEventKind::Down(MouseButton::Left),
            plan.layout.extension_blank.unwrap().x,
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
        assert_eq!(state.pointer.capture, None);
    }

    #[test]
    fn an_open_overlay_leaves_uncovered_dividers_operable() {
        let mut state = UiState::default();
        update(&mut state, UiEvent::Action(Action::OpenModels));
        let plan = draw(&state, 160);
        mouse(
            &mut state,
            &plan,
            MouseEventKind::Down(MouseButton::Left),
            31,
            0,
        );
        assert_eq!(
            state.pointer.capture,
            Some(PointerCapture::Divider(PaneDivider::Left))
        );
        let event = mapped_terminal_event(
            Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            Some(&plan),
            &state,
        );
        update(&mut state, event);
        assert_eq!(state.pointer.capture, None);
        assert!(state.overlay.is_some());
    }
}

#[cfg(test)]
mod input_chord_tests {
    use super::*;
    use crate::state::{Overlay, update};

    #[test]
    fn clear_is_undoable_empty_clear_is_inert_and_panels_protect_the_draft() {
        let mut state = UiState::default();
        state.orphan_draft = "中文 e\u{301}\nkeep me".into();
        let original = state.draft().to_owned();
        let clear = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        let action = mapped_key_action(clear, &state);
        assert!(matches!(
            action,
            Action::Edit {
                target: EditorTarget::Composer,
                command: EditCommand::Clear
            }
        ));
        assert!(update(&mut state, UiEvent::Action(action)).is_empty());
        assert!(state.draft().is_empty());
        assert!(!state.quitting);
        let revision = state.orphan_draft.revision();
        update(
            &mut state,
            UiEvent::Action(composer_edit(EditCommand::Clear)),
        );
        assert_eq!(state.orphan_draft.revision(), revision);
        update(
            &mut state,
            UiEvent::Action(composer_edit(EditCommand::Undo)),
        );
        assert_eq!(state.draft(), original);
        assert!(state.orphan_draft.revision() > revision);
        for focus in [WorkspaceTarget::Sessions, WorkspaceTarget::SessionTitle] {
            state.set_workspace_target(focus);
            assert!(test_key_action(clear, &state).is_none());
        }
        state.set_workspace_target(WorkspaceTarget::Composer);
        for panel in [model_list_panel(), Overlay::Help, model_setup_panel()] {
            state.overlay = Some(panel);
            state.enter_overlay();
            assert!(test_key_action(clear, &state).is_none());
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
            Action::Edit {
                target: EditorTarget::Composer,
                command: EditCommand::Insert { ref text, typing: false }
            } if text == "\n"
        ));
        assert!(
            test_key_action(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT), &state).is_none()
        );
        assert!(
            test_key_action(
                KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL),
                &state
            )
            .is_none()
        );
        assert!(
            test_key_action(
                KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL),
                &state
            )
            .is_none()
        );
        assert!(
            test_key_action(
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
        for focus in [WorkspaceTarget::Sessions, WorkspaceTarget::SessionTitle] {
            state.set_workspace_target(focus);
            assert!(
                test_key_action(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT), &state)
                    .is_none()
            );
        }
        state.set_workspace_target(WorkspaceTarget::Composer);
        for panel in [model_list_panel(), Overlay::Help, model_setup_panel()] {
            state.overlay = Some(panel);
            state.enter_overlay();
            for modifiers in [
                KeyModifiers::SHIFT,
                KeyModifiers::ALT,
                KeyModifiers::CONTROL,
            ] {
                assert!(
                    test_key_action(KeyEvent::new(KeyCode::Enter, modifiers), &state).is_none()
                );
            }
        }
    }

    #[test]
    fn ctrl_d_is_global_and_ctrl_p_and_ctrl_q_remain_unbound() {
        let mut state = UiState::default();
        for focus in [
            WorkspaceTarget::Composer,
            WorkspaceTarget::Sessions,
            WorkspaceTarget::SessionTitle,
        ] {
            state.set_workspace_target(focus);
            state.overlay = None;
            state.leave_overlay();
            assert!(matches!(
                mapped_key_action(
                    KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
                    &state
                ),
                Action::Quit
            ));
            assert!(
                test_key_action(
                    KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL),
                    &state
                )
                .is_none()
            );
            assert!(
                test_key_action(
                    KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
                    &state
                )
                .is_none()
            );
        }
        for panel in [model_list_panel(), Overlay::Help, model_setup_panel()] {
            state.overlay = Some(panel);
            state.enter_overlay();
            assert!(matches!(
                mapped_key_action(
                    KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
                    &state
                ),
                Action::Quit
            ));
            assert!(
                test_key_action(
                    KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL),
                    &state
                )
                .is_none()
            );
            assert!(
                test_key_action(
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

#[cfg(test)]
mod panel_keyboard_tests {
    use super::*;
    use crate::state::{ModelScreen, Overlay, SetupField, update};
    use ratatui::{Terminal, backend::TestBackend};

    fn key(state: &mut UiState, code: KeyCode) {
        let event = mapped_terminal_event(
            Event::Key(KeyEvent::new(code, KeyModifiers::NONE)),
            None,
            state,
        );
        update(state, event);
    }

    #[test]
    fn mouse_panel_keeps_typing_and_paste_in_composer_until_f6() {
        let mut state = UiState::default();
        state.orphan_draft = "before".into();
        update(&mut state, UiEvent::Action(Action::OpenModels));
        assert!(!matches!(state.keyboard, KeyboardOwner::Overlay { .. }));
        assert!(state.blinking_caret_active());
        assert!(crate::ui::focus::workspace_focused(
            &state,
            WorkspaceTarget::Composer
        ));
        key(&mut state, KeyCode::Char('!'));
        let paste = mapped_terminal_event(Event::Paste(" pasted".into()), None, &state);
        update(&mut state, paste);
        assert_eq!(state.draft(), "before! pasted");

        key(&mut state, KeyCode::F(6));
        assert!(matches!(state.keyboard, KeyboardOwner::Overlay { .. }));
        assert!(terminal_event(Event::Paste("ignored".into()), None, &state).is_none());
        key(&mut state, KeyCode::F(6));
        key(&mut state, KeyCode::Char('?'));
        assert_eq!(state.draft(), "before! pasted?");
        assert_eq!(state.workspace_target(), WorkspaceTarget::Composer);
    }

    #[test]
    fn setup_keyboard_ownership_controls_fields_and_secret_paste() {
        let mut state = UiState::default();
        update(&mut state, UiEvent::Action(Action::OpenModels));
        let Some(Overlay::Models(models)) = &mut state.overlay else {
            panic!("models")
        };
        models.screen = ModelScreen::Setup(Box::new(crate::state::ConnectionForm::new(
            crate::state::ConnectionKind::CustomOpenAiResponses,
        )));
        let paste = mapped_terminal_event(Event::Paste("draft".into()), None, &state);
        update(&mut state, paste);
        assert_eq!(state.draft(), "draft");
        key(&mut state, KeyCode::F(6));
        key(&mut state, KeyCode::Tab);
        key(&mut state, KeyCode::Tab);
        let paste = mapped_terminal_event(Event::Paste("private-key".into()), None, &state);
        update(&mut state, paste);
        let Some(Overlay::Models(models)) = &state.overlay else {
            panic!("models")
        };
        let form = models.setup().unwrap();
        assert_eq!(form.field, SetupField::Key);
        assert_eq!(form.key.as_str(), "private-key");
        assert_eq!(state.draft(), "draft");
        key(&mut state, KeyCode::F(6));
        key(&mut state, KeyCode::Char('!'));
        assert_eq!(state.draft(), "draft!");
    }

    #[test]
    fn actual_model_button_click_preserves_input_while_models_load() {
        let mut state = UiState::default();
        let mut terminal = Terminal::new(TestBackend::new(160, 40)).unwrap();
        let mut snapshot = None;
        terminal
            .draw(|frame| snapshot = Some(crate::view::render(frame, &state)))
            .unwrap();
        let snapshot = snapshot.unwrap();
        let button = snapshot
            .hit_regions()
            .into_iter()
            .find(|hit| hit.target == ClickTarget::Action(Action::OpenModels))
            .unwrap();
        for kind in [
            MouseEventKind::Down(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Left),
        ] {
            let event = mapped_terminal_event(
                Event::Mouse(crossterm::event::MouseEvent {
                    kind,
                    column: button.area.x,
                    row: button.area.y,
                    modifiers: KeyModifiers::NONE,
                }),
                Some(&snapshot),
                &state,
            );
            update(&mut state, event);
            if kind == MouseEventKind::Down(MouseButton::Left) {
                assert!(state.overlay.is_none(), "press does not open the menu");
            }
        }
        assert!(matches!(state.overlay, Some(Overlay::Models(_))));
        assert!(!matches!(state.keyboard, KeyboardOwner::Overlay { .. }));
        terminal
            .draw(|frame| {
                crate::view::render(frame, &state);
            })
            .unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("loading models…"));
        key(&mut state, KeyCode::Char('x'));
        assert_eq!(state.draft(), "x");
        key(&mut state, KeyCode::F(6));
        key(&mut state, KeyCode::Esc);
        assert!(state.overlay.is_none());
        key(&mut state, KeyCode::Char('y'));
        assert_eq!(state.draft(), "xy");
    }
}

#[cfg(test)]
mod independent_pointer_tests {
    use super::*;
    use crate::{
        layout::{PaneDivider, PaneWidths, SinglePane},
        state::{CommandKind, update},
    };
    use ratatui::{Terminal, backend::TestBackend, layout::Rect};

    fn draw(state: &UiState) -> FrameSnapshot {
        let mut terminal = Terminal::new(TestBackend::new(160, 40)).unwrap();
        let mut result = None;
        terminal
            .draw(|frame| result = Some(crate::view::render(frame, state)))
            .unwrap();
        result.unwrap()
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> Event {
        Event::Mouse(crossterm::event::MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        })
    }

    #[test]
    fn scrolling_a_clickable_child_uses_its_surface_scroll_region() {
        let layout = LayoutPlan::calculate_with_widths(
            Rect::new(0, 0, 160, 40),
            SinglePane::Conversation,
            0,
            None,
            None,
            1,
            PaneWidths::default(),
        );
        let area = layout.transcript.unwrap();
        let mut workspace = SurfaceHits::default();
        workspace.push_scroll(area, ScrollTarget::Conversation);
        workspace.push(ClickRegion {
            area,
            target: ClickTarget::Action(Action::OpenModels),
        });
        let overlay_area = Rect::new(area.x + 2, area.y + 2, 10, 5);
        let snapshot = FrameSnapshot::new(
            layout,
            FrameHits::new(workspace, Some((overlay_area, SurfaceHits::default()))),
            None,
            0,
            None,
            None,
        );
        let state = UiState::default();
        assert!(matches!(
            terminal_event(
                mouse(MouseEventKind::ScrollDown, area.x, area.y),
                Some(&snapshot),
                &state
            ),
            Some(UiEvent::Action(Action::ScrollDown(3))),
        ));
        assert!(
            terminal_event(
                mouse(MouseEventKind::ScrollDown, overlay_area.x, overlay_area.y),
                Some(&snapshot),
                &state,
            )
            .is_none()
        );
    }

    #[test]
    fn divider_capture_survives_typing_and_pointer_movement_until_mouse_up() {
        let mut state = UiState::default();
        let snapshot = draw(&state);
        update(
            &mut state,
            terminal_event(
                mouse(MouseEventKind::Down(MouseButton::Left), 31, 0),
                Some(&snapshot),
                &UiState::default(),
            )
            .unwrap(),
        );
        assert_eq!(
            state.pointer.capture,
            Some(PointerCapture::Divider(PaneDivider::Left))
        );
        let key = terminal_event(
            Event::Key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)),
            Some(&snapshot),
            &state,
        )
        .unwrap();
        update(&mut state, key);
        update(
            &mut state,
            UiEvent::PointerMoved {
                column: 80,
                row: 20,
            },
        );
        update(&mut state, UiEvent::RefreshOverviewRequested);
        assert_eq!(
            state.pointer.capture,
            Some(PointerCapture::Divider(PaneDivider::Left))
        );
        let drag = terminal_event(
            mouse(MouseEventKind::Drag(MouseButton::Left), 45, 20),
            Some(&snapshot),
            &state,
        )
        .unwrap();
        update(&mut state, drag);
        let release = terminal_event(
            mouse(MouseEventKind::Up(MouseButton::Left), 45, 20),
            Some(&snapshot),
            &state,
        )
        .unwrap();
        update(&mut state, release);
        assert_eq!(state.pointer.capture, None);
        assert_eq!(state.draft(), "a");
        assert_eq!(
            state.keyboard,
            KeyboardOwner::Workspace(WorkspaceTarget::Composer)
        );
    }

    #[test]
    fn editor_capture_drags_outside_its_hit_region_and_keyboard_switch_cancels_it() {
        let mut state = UiState::default();
        state.orphan_draft = "abcdef".into();
        let snapshot = draw(&state);
        let area = crate::layout::composer_text_area(snapshot.layout.composer.unwrap());
        let down = terminal_event(
            mouse(MouseEventKind::Down(MouseButton::Left), area.x + 1, area.y),
            Some(&snapshot),
            &state,
        )
        .unwrap();
        update(&mut state, down);
        assert_eq!(
            state.pointer.capture,
            Some(PointerCapture::Editor(EditorTarget::Composer))
        );
        let drag = terminal_event(
            mouse(MouseEventKind::Drag(MouseButton::Left), 159, 39),
            Some(&snapshot),
            &state,
        )
        .unwrap();
        update(&mut state, drag);
        assert_eq!(state.editor().selection(), Some(1..6));
        let switch = terminal_event(
            Event::Key(KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL)),
            Some(&snapshot),
            &state,
        )
        .unwrap();
        update(&mut state, switch);
        assert_eq!(state.pointer.capture, None);
        assert_eq!(
            state.keyboard,
            KeyboardOwner::Workspace(WorkspaceTarget::Sessions)
        );
    }

    #[test]
    fn mouse_command_candidate_prepares_draft_and_only_enter_executes() {
        let mut state = UiState::default();
        state.orphan_draft = "/he".into();
        let snapshot = draw(&state);
        let candidate = snapshot
            .hit_regions()
            .into_iter()
            .find(|region| {
                region.target == ClickTarget::Action(Action::PrepareCommand(CommandKind::Help))
            })
            .expect("help command candidate");
        for kind in [
            MouseEventKind::Down(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Left),
        ] {
            let event = terminal_event(
                mouse(kind, candidate.area.x, candidate.area.y),
                Some(&snapshot),
                &state,
            )
            .unwrap();
            update(&mut state, event);
            if kind == MouseEventKind::Down(MouseButton::Left) {
                assert_eq!(state.draft(), "/he", "press does not complete the command");
            }
        }
        assert!(state.overlay.is_none());
        assert_eq!(
            state.keyboard,
            KeyboardOwner::Workspace(WorkspaceTarget::Composer)
        );
        assert_eq!(state.draft().trim(), "/help");
        let snapshot = draw(&state);
        assert!(
            !snapshot
                .hit_regions()
                .into_iter()
                .any(|region| region.target == ClickTarget::Action(Action::Submit))
        );
        let enter = terminal_event(
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Some(&snapshot),
            &state,
        )
        .unwrap();
        update(&mut state, enter);
        assert!(matches!(state.overlay, Some(crate::state::Overlay::Help)));
        assert!(matches!(state.keyboard, KeyboardOwner::Overlay { .. }));
    }

    #[test]
    fn too_small_blocks_form_paste_and_keeps_owner_while_focus_loss_clears_pointer() {
        let mut state = UiState::default();
        state.overlay = Some(model_setup_panel());
        state.enter_overlay();
        let owner = state.keyboard;
        let snapshot = frame_for_layout(LayoutPlan::calculate_with_widths(
            Rect::new(0, 0, 20, 5),
            SinglePane::Conversation,
            0,
            None,
            None,
            1,
            PaneWidths::default(),
        ));
        assert!(terminal_event(Event::Paste("secret".into()), Some(&snapshot), &state).is_none());
        assert_eq!(state.keyboard, owner);
        update(&mut state, UiEvent::PointerMoved { column: 5, row: 2 });
        let lost = terminal_event(Event::FocusLost, Some(&snapshot), &state).unwrap();
        update(&mut state, lost);
        assert_eq!(state.pointer.position, None);
        assert_eq!(state.keyboard, owner);
    }
}
