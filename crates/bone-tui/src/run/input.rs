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
                MouseEventKind::ScrollUp
                    if matches!(target, Some(HitTarget::SessionRail | HitTarget::Session(_))) =>
                {
                    Action::SelectPrevious
                }
                MouseEventKind::ScrollDown
                    if matches!(target, Some(HitTarget::SessionRail | HitTarget::Session(_))) =>
                {
                    Action::SelectNext
                }
                MouseEventKind::ScrollUp => Action::ScrollUp {
                    amount: 3,
                    metrics: layout.and_then(|plan| plan.transcript_metrics.clone()),
                },
                MouseEventKind::ScrollDown => Action::ScrollDown(3),
                MouseEventKind::Down(MouseButton::Left) => hit_action(target, state),
                _ => Action::Noop,
            }
        }
        Event::Resize(_, _) => return UiEvent::Resized,
        Event::Paste(text) => Action::Paste(text),
        _ => return UiEvent::Tick,
    };
    let action = match action {
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
            _ => Action::Noop,
        };
    }
    let slash_open = !state.slash_matches().is_empty() && state.focus == Focus::Composer;
    match key.code {
        KeyCode::Esc => Action::Escape,
        KeyCode::Up if slash_open => Action::SelectSlashPrevious,
        KeyCode::Down if slash_open => Action::SelectSlashNext,
        KeyCode::Tab if slash_open => Action::CompleteSlash,
        KeyCode::Up if state.focus == Focus::Sessions => Action::SelectPrevious,
        KeyCode::Down if state.focus == Focus::Sessions => Action::SelectNext,
        KeyCode::Up | KeyCode::PageUp if state.focus == Focus::Conversation => Action::ScrollUp {
            amount: if key.code == KeyCode::PageUp { 10 } else { 1 },
            metrics: None,
        },
        KeyCode::Down | KeyCode::PageDown if state.focus == Focus::Conversation => {
            Action::ScrollDown(if key.code == KeyCode::PageDown { 10 } else { 1 })
        }
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
