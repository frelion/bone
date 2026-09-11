use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};

use crate::{
    layout::{HitTarget, LayoutPlan},
    state::{Action, Focus, UiEvent, UiState},
};

pub(super) fn terminal_event(
    event: Event,
    layout: Option<&LayoutPlan>,
    state: &UiState,
) -> UiEvent {
    let action = match event {
        Event::Key(key) if key.kind == KeyEventKind::Press => key_action(key, state),
        Event::Mouse(mouse) => {
            let target = layout.and_then(|plan| plan.hit(mouse.column, mouse.row));
            match mouse.kind {
                MouseEventKind::ScrollUp if target == Some(HitTarget::Reader) => {
                    Action::ScrollPanel { amount: -3, max: 0 }
                }
                MouseEventKind::ScrollDown if target == Some(HitTarget::Reader) => {
                    Action::ScrollPanel { amount: 3, max: 0 }
                }
                MouseEventKind::ScrollUp
                    if matches!(target, Some(HitTarget::SessionRail | HitTarget::Session(_))) =>
                {
                    layout.map_or(Action::Noop, |plan| Action::ScrollSessions {
                        start: plan.session_start.saturating_sub(3),
                    })
                }
                MouseEventKind::ScrollDown
                    if matches!(target, Some(HitTarget::SessionRail | HitTarget::Session(_))) =>
                {
                    layout.map_or(Action::Noop, |plan| Action::ScrollSessions {
                        start: plan
                            .session_start
                            .saturating_add(3)
                            .min(plan.session_max_start),
                    })
                }
                MouseEventKind::ScrollUp if target == Some(HitTarget::Conversation) => {
                    Action::ScrollUp {
                        amount: 3,
                        metrics: layout.and_then(|plan| plan.transcript_metrics.clone()),
                    }
                }
                MouseEventKind::ScrollDown if target == Some(HitTarget::Conversation) => {
                    Action::ScrollDown(3)
                }
                MouseEventKind::Down(MouseButton::Left)
                | MouseEventKind::Drag(MouseButton::Left)
                    if target == Some(HitTarget::Composer) && state.panel.is_none() =>
                {
                    let area = crate::layout::composer_text_area(layout.unwrap().composer.unwrap());
                    let byte = crate::text::cursor_at_origin(
                        state.draft(),
                        area.width,
                        state.editor().viewport.get(),
                        mouse
                            .column
                            .saturating_sub(area.x)
                            .min(area.width.saturating_sub(1)),
                        mouse
                            .row
                            .saturating_sub(area.y)
                            .min(area.height.saturating_sub(1)),
                    );
                    if matches!(mouse.kind, MouseEventKind::Drag(_))
                        || mouse.modifiers.contains(KeyModifiers::SHIFT)
                    {
                        Action::DragCursor(byte)
                    } else {
                        Action::PlaceCursor(byte)
                    }
                }
                MouseEventKind::Down(MouseButton::Left) => hit_action(target, state),
                _ => Action::Noop,
            }
        }
        Event::Resize(_, _) => return UiEvent::Resized,
        Event::Paste(text) if matches!(state.panel, Some(crate::state::Panel::ModelSetup)) => {
            Action::SetupText(text.into())
        }
        Event::Paste(text) if matches!(state.panel, Some(crate::state::Panel::Rename)) => {
            Action::RenameText(text)
        }
        Event::Paste(_) if state.panel.is_some() => Action::Noop,
        Event::Paste(text) => Action::Paste(text),
        _ => return UiEvent::Tick,
    };
    if layout.is_some_and(|plan| plan.mode == crate::layout::LayoutMode::TooSmall)
        && !matches!(action, Action::Quit | Action::Terminate)
    {
        return UiEvent::Tick;
    }
    let action = match action {
        Action::ScrollPanel { amount, .. } => Action::ScrollPanel {
            amount,
            max: layout.map_or(0, |plan| plan.reader_max_scroll),
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
            width: layout
                .and_then(|plan| plan.composer)
                .map_or(1, |area| crate::layout::composer_text_area(area).width),
        },
        Action::CursorVertical { down, .. } => Action::CursorVertical {
            down,
            width: layout
                .and_then(|plan| plan.composer)
                .map_or(1, |area| area.width.saturating_sub(6)),
        },
        Action::ScrollUp {
            amount,
            metrics: None,
        } => Action::ScrollUp {
            amount,
            metrics: layout.and_then(|plan| plan.transcript_metrics.clone()),
        },
        action => action,
    };
    UiEvent::Action(action)
}

pub(crate) fn key_action(key: KeyEvent, state: &UiState) -> Action {
    if state.panel.is_some() {
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('q' | 'c'))
        {
            return Action::Quit;
        }
        if matches!(state.panel, Some(crate::state::Panel::ModelSetup)) {
            return match key.code {
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    Action::SetupClear
                }
                KeyCode::Esc => Action::Escape,
                KeyCode::BackTab | KeyCode::Up => Action::PreviousField,
                KeyCode::Tab | KeyCode::Down => Action::NextField,
                KeyCode::Enter => Action::SaveConnection,
                KeyCode::Backspace => Action::SetupBackspace,
                KeyCode::Char(ch)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    Action::SetupText(ch.to_string().into())
                }
                _ => Action::Noop,
            };
        }
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return Action::Noop;
        }
        return match key.code {
            KeyCode::Char(ch) if matches!(state.panel, Some(crate::state::Panel::Rename)) => {
                Action::RenameText(ch.to_string())
            }
            KeyCode::Backspace if matches!(state.panel, Some(crate::state::Panel::Rename)) => {
                Action::RenameBackspace
            }
            KeyCode::Esc => Action::Escape,
            KeyCode::Up | KeyCode::PageUp
                if matches!(state.panel, Some(crate::state::Panel::Reader(_))) =>
            {
                Action::ScrollPanel {
                    amount: if key.code == KeyCode::PageUp { -10 } else { -1 },
                    max: 0,
                }
            }
            KeyCode::Down | KeyCode::PageDown
                if matches!(state.panel, Some(crate::state::Panel::Reader(_))) =>
            {
                Action::ScrollPanel {
                    amount: if key.code == KeyCode::PageDown { 10 } else { 1 },
                    max: 0,
                }
            }
            KeyCode::Up => Action::PanelPrevious,
            KeyCode::Down => Action::PanelNext,
            KeyCode::Enter => Action::ActivatePanel,
            _ => Action::Noop,
        };
    }
    if key.code == KeyCode::Enter
        && (key.modifiers.contains(KeyModifiers::SHIFT)
            || key.modifiers.contains(KeyModifiers::ALT))
    {
        return Action::InsertNewline;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return match key.code {
            KeyCode::Left => Action::FocusLeft,
            KeyCode::Right => Action::FocusRight,
            KeyCode::Up => Action::FocusUp,
            KeyCode::Down => Action::FocusDown,
            KeyCode::Char('c' | 'q') => Action::Quit,
            KeyCode::Char('p') => Action::OpenCommands,
            KeyCode::Char('z') if state.focus == Focus::Composer => Action::Undo,
            KeyCode::Char('y') if state.focus == Focus::Composer => Action::Redo,
            _ => Action::Noop,
        };
    }
    if state.focus == Focus::Composer
        && key
            .modifiers
            .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT)
    {
        let direction = match key.code {
            KeyCode::Left => -1,
            KeyCode::Right => 1,
            KeyCode::Up => -2,
            KeyCode::Down => 2,
            KeyCode::Home => -3,
            KeyCode::End => 3,
            _ => 0,
        };
        if direction != 0 {
            return Action::MoveCursor {
                direction,
                width: 1,
                select: key.modifiers.contains(KeyModifiers::SHIFT),
                word: key.modifiers.contains(KeyModifiers::ALT),
            };
        }
        if key.modifiers.contains(KeyModifiers::ALT) {
            return Action::Noop;
        }
    }
    let slash_open = !state.slash_matches().is_empty() && state.focus == Focus::Composer;
    match key.code {
        KeyCode::Esc => Action::Escape,
        KeyCode::Up if slash_open => Action::SelectSlashPrevious,
        KeyCode::Down if slash_open => Action::SelectSlashNext,
        KeyCode::Tab if slash_open => Action::CompleteSlash,
        KeyCode::Up if state.focus == Focus::Sessions => Action::SelectPrevious,
        KeyCode::Down if state.focus == Focus::Sessions => Action::SelectNext,
        KeyCode::Enter if state.focus == Focus::Sessions => Action::OpenCandidate,
        KeyCode::PageUp if state.focus == Focus::Composer => Action::ScrollUp {
            amount: 10,
            metrics: None,
        },
        KeyCode::PageDown if state.focus == Focus::Composer => Action::ScrollDown(10),
        KeyCode::End if state.focus == Focus::Conversation => Action::ScrollDown(usize::MAX),
        KeyCode::Up | KeyCode::PageUp if state.focus == Focus::Conversation => Action::ScrollUp {
            amount: if key.code == KeyCode::PageUp { 10 } else { 1 },
            metrics: None,
        },
        KeyCode::Down | KeyCode::PageDown if state.focus == Focus::Conversation => {
            Action::ScrollDown(if key.code == KeyCode::PageDown { 10 } else { 1 })
        }
        KeyCode::Up | KeyCode::Down if state.focus == Focus::Composer => Action::CursorVertical {
            down: key.code == KeyCode::Down,
            width: 1,
        },
        KeyCode::Enter if state.focus == Focus::Composer => Action::Submit,
        KeyCode::Backspace if state.focus == Focus::Composer => Action::Backspace,
        KeyCode::Delete if state.focus == Focus::Composer => Action::Delete,
        KeyCode::Left if state.focus == Focus::Composer => Action::CursorLeft,
        KeyCode::Right if state.focus == Focus::Composer => Action::CursorRight,
        KeyCode::Home if state.focus == Focus::Composer => Action::CursorHome,
        KeyCode::End if state.focus == Focus::Composer => Action::CursorEnd,
        KeyCode::Char(value) if state.focus == Focus::Composer => Action::Input(value),
        _ => Action::Noop,
    }
}

fn hit_action(target: Option<HitTarget>, state: &UiState) -> Action {
    match target {
        Some(HitTarget::Session(index)) => state
            .sessions
            .get(index)
            .map_or(Action::Noop, |session| Action::SelectSession(session.id)),
        Some(HitTarget::SessionRail) => Action::Focus(Focus::Sessions),
        Some(HitTarget::Conversation) => Action::Focus(Focus::Conversation),
        Some(HitTarget::Composer) => Action::Focus(Focus::Composer),
        Some(HitTarget::SlashCommand(index)) => Action::ExecuteSlash(index),
        Some(HitTarget::NewSession) => Action::NewSession,
        Some(HitTarget::Commands) => Action::OpenCommands,
        Some(HitTarget::Models) => Action::OpenModels,
        Some(HitTarget::ConnectionKind(index)) => Action::ChooseConnectionKind(index),
        Some(HitTarget::SetupField(field)) => Action::SelectField(field),
        Some(HitTarget::SaveConnection) => Action::SaveConnection,
        Some(HitTarget::Back) => Action::Escape,
        Some(HitTarget::Reader) => Action::Noop,
        Some(HitTarget::Model(index)) => Action::SelectModel(index),
        Some(HitTarget::Object(index)) => Action::SelectObject(index),
        Some(HitTarget::History(sequence)) => Action::OpenHistory(sequence),
        Some(HitTarget::Job(job)) => Action::OpenJob(job),
        Some(HitTarget::Answer(question)) => Action::AnswerQuestion(question),
        Some(HitTarget::LeaveAnswer) => Action::LeaveAnswer,
        Some(HitTarget::ConvertAnswer) => Action::ConvertAnswer,
        Some(HitTarget::Restore(input)) => Action::RestoreInput(input),
        Some(HitTarget::Retry(input)) => Action::RetryInput(input),
        Some(HitTarget::RetrySubmission) => Action::RetrySubmission,
        Some(HitTarget::Submit) => Action::ClickSubmit,
        Some(HitTarget::Stop) => Action::Stop,
        None => Action::Noop,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn composer_pages_read_history_and_conversation_end_follows_tail() {
        let mut state = UiState::default();
        assert!(matches!(
            key_action(key(KeyCode::PageUp, KeyModifiers::NONE), &state),
            Action::ScrollUp { .. }
        ));
        assert!(matches!(
            key_action(key(KeyCode::PageDown, KeyModifiers::NONE), &state),
            Action::ScrollDown(10)
        ));
        state.focus = Focus::Conversation;
        assert!(matches!(
            key_action(key(KeyCode::End, KeyModifiers::NONE), &state),
            Action::ScrollDown(usize::MAX)
        ));
    }

    #[test]
    fn ctrl_arrows_are_the_only_spatial_focus_keys() {
        let state = UiState::default();
        assert!(matches!(
            key_action(key(KeyCode::Left, KeyModifiers::CONTROL), &state),
            Action::FocusLeft
        ));
        assert!(matches!(
            key_action(key(KeyCode::Right, KeyModifiers::CONTROL), &state),
            Action::FocusRight
        ));
        assert!(matches!(
            key_action(key(KeyCode::Up, KeyModifiers::CONTROL), &state),
            Action::FocusUp
        ));
        assert!(matches!(
            key_action(key(KeyCode::Down, KeyModifiers::CONTROL), &state),
            Action::FocusDown
        ));
        assert!(matches!(
            key_action(key(KeyCode::Tab, KeyModifiers::NONE), &state),
            Action::Noop
        ));
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
        assert!(matches!(
            terminal_event(
                Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
                Some(&plan),
                &state
            ),
            UiEvent::Tick
        ));
        assert!(matches!(
            terminal_event(
                Event::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL)),
                Some(&plan),
                &state
            ),
            UiEvent::Action(Action::Quit)
        ));
        assert!(matches!(
            terminal_event(Event::Resize(80, 24), Some(&plan), &state),
            UiEvent::Resized
        ));
    }
}

#[cfg(test)]
mod model_keyboard_tests {
    use super::*;

    #[test]
    fn rename_editing_and_paste_stay_in_the_panel() {
        let mut state = UiState::default();
        state.panel = Some(crate::state::Panel::Rename);
        assert!(
            matches!(key_action(KeyEvent::new(KeyCode::Char('名'), KeyModifiers::NONE), &state), Action::RenameText(text) if text == "名")
        );
        assert!(matches!(
            key_action(
                KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
                &state
            ),
            Action::RenameBackspace
        ));
        assert!(matches!(
            key_action(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &state),
            Action::ActivatePanel
        ));
        assert!(
            matches!(terminal_event(Event::Paste("新标题".into()), None, &state), UiEvent::Action(Action::RenameText(text)) if text == "新标题")
        );
        assert!(matches!(
            key_action(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &state),
            Action::Escape
        ));
    }

    #[test]
    fn command_chords_do_not_become_model_text() {
        let mut state = UiState::default();
        state.panel = Some(crate::state::Panel::Models);
        for modifiers in [KeyModifiers::CONTROL, KeyModifiers::ALT] {
            assert!(matches!(
                key_action(KeyEvent::new(KeyCode::Char('a'), modifiers), &state),
                Action::Noop
            ));
        }
        assert!(matches!(
            key_action(
                KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT),
                &state
            ),
            Action::Noop
        ));
        assert!(matches!(
            key_action(
                KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL),
                &state
            ),
            Action::Quit
        ));
    }
    #[test]
    fn connection_form_keys_and_paste_use_secret_safe_actions() {
        let mut state = UiState::default();
        state.panel = Some(crate::state::Panel::ModelSetup);
        let event = terminal_event(Event::Paste("private-key-test".into()), None, &state);
        assert!(!format!("{event:?}").contains("private-key-test"));
        assert!(
            matches!(event, UiEvent::Action(Action::SetupText(text)) if text.as_str() == "private-key-test")
        );
        assert!(matches!(
            key_action(
                KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
                &state
            ),
            Action::SetupClear
        ));
        assert!(matches!(
            key_action(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &state),
            Action::NextField
        ));
    }

    #[test]
    fn clicking_visible_composer_moves_insertion_without_editing_draft() {
        let mut state = UiState::default();
        state.orphan_draft = "ab中文".into();
        state.orphan_cursor = state.orphan_draft.len();
        state.focus = Focus::Conversation;
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        let mut plan = None;
        terminal
            .draw(|frame| plan = Some(crate::view::render(frame, &state)))
            .unwrap();
        let plan = plan.unwrap();
        let text = crate::layout::composer_text_area(plan.composer.unwrap());
        let event = terminal_event(
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
            key_action(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &state),
            Action::SelectNext
        ));
        assert!(matches!(
            key_action(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &state),
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
        let event = Event::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 2,
            row: 4,
            modifiers: KeyModifiers::NONE,
        });
        assert!(matches!(
            terminal_event(event, Some(&plan), &state),
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
                matches!(key_action(KeyEvent::new(key, KeyModifiers::SHIFT), &state), Action::MoveCursor { direction, select: true, .. } if direction == expected)
            );
        }
        assert!(matches!(
            key_action(KeyEvent::new(KeyCode::Right, KeyModifiers::ALT), &state),
            Action::MoveCursor {
                word: true,
                select: false,
                ..
            }
        ));
        assert!(matches!(
            key_action(
                KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL),
                &state
            ),
            Action::Undo
        ));
        assert!(matches!(
            key_action(
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
            let event = terminal_event(
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
}

#[cfg(test)]
mod pointer_submit_tests {
    use super::*;
    #[test]
    fn visible_submit_click_is_distinct_from_reading_enter() {
        let mut state = UiState::default();
        state.orphan_draft = "send this".into();
        state.focus = Focus::Conversation;
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        let mut plan = None;
        terminal
            .draw(|frame| plan = Some(crate::view::render(frame, &state)))
            .unwrap();
        let plan = plan.unwrap();
        let submit = plan
            .hit_regions
            .iter()
            .find(|region| region.target == HitTarget::Submit)
            .unwrap();
        assert!(matches!(
            terminal_event(
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
        assert!(matches!(
            key_action(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &state),
            Action::Noop
        ));
    }
}
