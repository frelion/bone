//! Pure keyboard mapping.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::state::{Action, Focus, Panel, UiState};

/// One shortcut rendered in the composer's status baseline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct BindingHint {
    pub(crate) chord: &'static str,
    pub(crate) label: &'static str,
}

/// The visible shortcut contract. Rendering code consumes this list directly so
/// help text cannot quietly acquire aliases that the keymap does not support.
pub(crate) const STATUS_BASELINE_BINDINGS: [BindingHint; 3] = [
    BindingHint {
        chord: "shift+enter",
        label: "newline",
    },
    BindingHint {
        chord: "ctrl+c",
        label: "clear",
    },
    BindingHint {
        chord: "ctrl+d",
        label: "exit",
    },
];

pub(crate) fn status_baseline_bindings(shift_enter_supported: bool) -> [BindingHint; 3] {
    let mut bindings = STATUS_BASELINE_BINDINGS;
    if !shift_enter_supported {
        bindings[0].label = "unavailable";
    }
    bindings
}

pub(super) fn key_action(key: KeyEvent, state: &UiState) -> Option<Action> {
    // Exit is global, including modal panels and the too-small fallback screen.
    if exact(&key, KeyCode::Char('d'), KeyModifiers::CONTROL) {
        return Some(Action::Quit);
    }

    // Enter chords are exact. This guard prevents a modified Enter from
    // activating a panel or a session after falling through another branch.
    if key.code == KeyCode::Enter && key.modifiers != KeyModifiers::NONE {
        return (state.panel.is_none()
            && state.focus == Focus::Composer
            && key.modifiers == KeyModifiers::SHIFT)
            .then_some(Action::InsertNewline);
    }

    if let Some(panel) = &state.panel {
        return panel_action(key, panel);
    }

    if key.modifiers.contains(KeyModifiers::CONTROL) {
        // Control chords are exact. Extra modifiers must not turn into spatial
        // focus movement or fall through as ordinary editor input.
        if key.modifiers != KeyModifiers::CONTROL {
            return None;
        }
        return match key.code {
            KeyCode::Left => Some(Action::FocusLeft),
            KeyCode::Right => Some(Action::FocusRight),
            KeyCode::Up => Some(Action::FocusUp),
            KeyCode::Down => Some(Action::FocusDown),
            KeyCode::Char('c') if state.focus == Focus::Composer => Some(Action::ClearInput),
            KeyCode::Char('z') if state.focus == Focus::Composer => Some(Action::Undo),
            KeyCode::Char('y') if state.focus == Focus::Composer => Some(Action::Redo),
            KeyCode::Char('z') if state.focus == Focus::SessionTitle => Some(Action::TitleUndo),
            KeyCode::Char('y') if state.focus == Focus::SessionTitle => Some(Action::TitleRedo),
            _ => None,
        };
    }

    if state.focus == Focus::SessionTitle
        && key
            .modifiers
            .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT)
    {
        let direction = match key.code {
            KeyCode::Left => -1,
            KeyCode::Right => 1,
            KeyCode::Home if key.modifiers == KeyModifiers::SHIFT => -3,
            KeyCode::End if key.modifiers == KeyModifiers::SHIFT => 3,
            _ => 0,
        };
        if direction != 0 {
            return Some(Action::TitleMoveCursor {
                direction,
                select: key.modifiers.contains(KeyModifiers::SHIFT),
                word: key.modifiers.contains(KeyModifiers::ALT),
            });
        }
        if key.modifiers.contains(KeyModifiers::ALT) {
            return None;
        }
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
            KeyCode::Home if key.modifiers == KeyModifiers::SHIFT => -3,
            KeyCode::End if key.modifiers == KeyModifiers::SHIFT => 3,
            _ => 0,
        };
        if direction != 0 {
            return Some(Action::MoveCursor {
                direction,
                width: 1,
                select: key.modifiers.contains(KeyModifiers::SHIFT),
                word: key.modifiers.contains(KeyModifiers::ALT),
            });
        }
        if key.modifiers.contains(KeyModifiers::ALT) {
            return None;
        }
    }

    let slash_open = state.slash_palette_visible();
    match key.code {
        KeyCode::Esc if state.focus == Focus::SessionTitle => Some(Action::CancelTitle),
        KeyCode::Esc => Some(Action::Escape),
        KeyCode::Up if slash_open => Some(Action::SelectSlashPrevious),
        KeyCode::Down if slash_open => Some(Action::SelectSlashNext),
        KeyCode::Tab if slash_open => Some(Action::CompleteSlash),
        KeyCode::Up if state.focus == Focus::Sessions => Some(Action::SelectPrevious),
        KeyCode::Down if state.focus == Focus::Sessions => Some(Action::SelectNext),
        KeyCode::Enter if state.focus == Focus::Sessions => Some(Action::OpenCandidate),
        KeyCode::PageUp if state.focus == Focus::Composer => Some(Action::ScrollUp {
            amount: 10,
            metrics: None,
        }),
        KeyCode::PageDown if state.focus == Focus::Composer => Some(Action::ScrollDown(10)),
        KeyCode::PageUp if state.focus == Focus::SessionTitle => Some(Action::ScrollUp {
            amount: 10,
            metrics: None,
        }),
        KeyCode::PageDown if state.focus == Focus::SessionTitle => Some(Action::ScrollDown(10)),
        KeyCode::Enter if state.focus == Focus::SessionTitle => Some(Action::CommitTitle),
        KeyCode::Backspace if state.focus == Focus::SessionTitle => Some(Action::TitleBackspace),
        KeyCode::Delete if state.focus == Focus::SessionTitle => Some(Action::TitleDelete),
        KeyCode::Left if state.focus == Focus::SessionTitle => Some(Action::TitleMoveCursor {
            direction: -1,
            select: false,
            word: false,
        }),
        KeyCode::Right if state.focus == Focus::SessionTitle => Some(Action::TitleMoveCursor {
            direction: 1,
            select: false,
            word: false,
        }),
        KeyCode::Home if state.focus == Focus::SessionTitle => Some(Action::TitleHome),
        KeyCode::End if state.focus == Focus::SessionTitle => Some(Action::TitleEnd),
        KeyCode::Char(value) if state.focus == Focus::SessionTitle => {
            Some(Action::TitleInput(value))
        }
        KeyCode::Up | KeyCode::Down if state.focus == Focus::Composer => {
            Some(Action::CursorVertical {
                down: key.code == KeyCode::Down,
                width: 1,
            })
        }
        KeyCode::Enter if state.focus == Focus::Composer => Some(Action::Submit),
        KeyCode::Backspace if state.focus == Focus::Composer => Some(Action::Backspace),
        KeyCode::Delete if state.focus == Focus::Composer => Some(Action::Delete),
        KeyCode::Left if state.focus == Focus::Composer => Some(Action::CursorLeft),
        KeyCode::Right if state.focus == Focus::Composer => Some(Action::CursorRight),
        KeyCode::Home if state.focus == Focus::Composer => Some(Action::CursorHome),
        KeyCode::End if state.focus == Focus::Composer => Some(Action::CursorEnd),
        KeyCode::Char(value) if state.focus == Focus::Composer => Some(Action::Input(value)),
        _ => None,
    }
}

fn exact(key: &KeyEvent, code: KeyCode, modifiers: KeyModifiers) -> bool {
    key.code == code && key.modifiers == modifiers
}

fn panel_action(key: KeyEvent, panel: &Panel) -> Option<Action> {
    if matches!(panel, Panel::ModelSetup) {
        if exact(&key, KeyCode::Char('u'), KeyModifiers::CONTROL) {
            return Some(Action::SetupClear);
        }
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return None;
        }
        return match key.code {
            KeyCode::Esc => Some(Action::Escape),
            KeyCode::BackTab | KeyCode::Up => Some(Action::PreviousField),
            KeyCode::Tab | KeyCode::Down => Some(Action::NextField),
            KeyCode::Enter => Some(Action::SaveConnection),
            KeyCode::Backspace => Some(Action::SetupBackspace),
            KeyCode::Char(ch)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                Some(Action::SetupText(ch.to_string().into()))
            }
            _ => None,
        };
    }

    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return None;
    }
    match key.code {
        KeyCode::Esc => Some(Action::Escape),
        KeyCode::Up | KeyCode::PageUp if matches!(panel, Panel::Reader(_)) => {
            Some(Action::ScrollPanel {
                amount: if key.code == KeyCode::PageUp { -10 } else { -1 },
                max: 0,
            })
        }
        KeyCode::Down | KeyCode::PageDown if matches!(panel, Panel::Reader(_)) => {
            Some(Action::ScrollPanel {
                amount: if key.code == KeyCode::PageDown { 10 } else { 1 },
                max: 0,
            })
        }
        KeyCode::Up => Some(Action::PanelPrevious),
        KeyCode::Down => Some(Action::PanelNext),
        KeyCode::Enter => Some(Action::ActivatePanel),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn spatial_focus_requires_exact_control() {
        let state = UiState::default();
        for code in [KeyCode::Left, KeyCode::Right, KeyCode::Up, KeyCode::Down] {
            assert!(key_action(key(code, KeyModifiers::CONTROL), &state).is_some());
            for extra in [
                KeyModifiers::SHIFT,
                KeyModifiers::ALT,
                KeyModifiers::SHIFT | KeyModifiers::ALT,
            ] {
                assert!(
                    key_action(key(code, KeyModifiers::CONTROL | extra), &state).is_none(),
                    "{code:?} with {extra:?} must not move focus"
                );
            }
        }
    }

    #[test]
    fn model_setup_rejects_control_modified_navigation() {
        let mut state = UiState::default();
        state.panel = Some(Panel::ModelSetup);

        assert!(matches!(
            key_action(key(KeyCode::Char('u'), KeyModifiers::CONTROL), &state),
            Some(Action::SetupClear)
        ));
        assert!(
            key_action(
                key(
                    KeyCode::Char('u'),
                    KeyModifiers::CONTROL | KeyModifiers::SHIFT
                ),
                &state
            )
            .is_none()
        );
        for code in [KeyCode::Up, KeyCode::Down] {
            assert!(key_action(key(code, KeyModifiers::CONTROL), &state).is_none());
            assert!(
                key_action(
                    key(code, KeyModifiers::CONTROL | KeyModifiers::SHIFT),
                    &state
                )
                .is_none()
            );
        }
        assert!(matches!(
            key_action(key(KeyCode::Up, KeyModifiers::NONE), &state),
            Some(Action::PreviousField)
        ));
        assert!(matches!(
            key_action(key(KeyCode::Down, KeyModifiers::NONE), &state),
            Some(Action::NextField)
        ));
    }

    #[test]
    fn documented_input_chords_remain_exact_without_aliases() {
        let state = UiState::default();
        assert!(matches!(
            key_action(key(KeyCode::Enter, KeyModifiers::NONE), &state),
            Some(Action::Submit)
        ));
        assert!(matches!(
            key_action(key(KeyCode::Enter, KeyModifiers::SHIFT), &state),
            Some(Action::InsertNewline)
        ));
        assert!(matches!(
            key_action(key(KeyCode::Char('c'), KeyModifiers::CONTROL), &state),
            Some(Action::ClearInput)
        ));
        assert!(matches!(
            key_action(key(KeyCode::Char('d'), KeyModifiers::CONTROL), &state),
            Some(Action::Quit)
        ));

        for key in [
            key(KeyCode::Char('j'), KeyModifiers::CONTROL),
            key(KeyCode::Char('p'), KeyModifiers::CONTROL),
            key(KeyCode::Char('q'), KeyModifiers::CONTROL),
            key(KeyCode::Enter, KeyModifiers::ALT),
            key(KeyCode::Enter, KeyModifiers::SHIFT | KeyModifiers::CONTROL),
            key(
                KeyCode::Char('c'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            ),
            key(
                KeyCode::Char('d'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            ),
        ] {
            assert!(key_action(key, &state).is_none());
        }
    }

    #[test]
    fn an_unmatched_slash_query_still_owns_menu_navigation() {
        let mut state = UiState::default();
        state.orphan_draft = "/does-not-exist".into();
        state.orphan_cursor = state.orphan_draft.len();
        assert!(state.slash_palette_visible());
        assert!(state.slash_matches().is_empty());
        assert!(matches!(
            key_action(key(KeyCode::Up, KeyModifiers::NONE), &state),
            Some(Action::SelectSlashPrevious)
        ));
        assert!(matches!(
            key_action(key(KeyCode::Down, KeyModifiers::NONE), &state),
            Some(Action::SelectSlashNext)
        ));
    }
}
