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
        return match key.code {
            KeyCode::Left => Some(Action::FocusLeft),
            KeyCode::Right => Some(Action::FocusRight),
            KeyCode::Up => Some(Action::FocusUp),
            KeyCode::Down => Some(Action::FocusDown),
            KeyCode::Char('c')
                if state.focus == Focus::Composer && key.modifiers == KeyModifiers::CONTROL =>
            {
                Some(Action::ClearInput)
            }
            KeyCode::Char('p') => Some(Action::OpenCommands),
            KeyCode::Char('z') if state.focus == Focus::Composer => Some(Action::Undo),
            KeyCode::Char('y') if state.focus == Focus::Composer => Some(Action::Redo),
            _ => None,
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

    let slash_open = !state.slash_matches().is_empty() && state.focus == Focus::Composer;
    match key.code {
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
        KeyCode::End if state.focus == Focus::Conversation => Some(Action::FollowTail),
        KeyCode::Up | KeyCode::PageUp if state.focus == Focus::Conversation => {
            Some(Action::ScrollUp {
                amount: if key.code == KeyCode::PageUp { 10 } else { 1 },
                metrics: None,
            })
        }
        KeyCode::Down | KeyCode::PageDown if state.focus == Focus::Conversation => {
            Some(Action::ScrollDown(if key.code == KeyCode::PageDown {
                10
            } else {
                1
            }))
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
        return match key.code {
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(Action::SetupClear)
            }
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
        KeyCode::Char(ch) if matches!(panel, Panel::Rename) => {
            Some(Action::RenameText(ch.to_string()))
        }
        KeyCode::Backspace if matches!(panel, Panel::Rename) => Some(Action::RenameBackspace),
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
