//! Resolves a composer's Enter without giving business submission input semantics.

use crate::state::{Action, COMMANDS, UiState};

pub(crate) fn submit_action(state: &UiState) -> Action {
    let answering = state
        .selected_ui()
        .is_some_and(|ui| ui.selected_answer.is_some());
    let Some(raw) = state
        .draft()
        .trim_start()
        .strip_prefix('/')
        .filter(|_| !answering)
    else {
        return Action::Submit;
    };
    let mut parts = raw.trim().splitn(2, char::is_whitespace);
    let typed = parts.next().unwrap_or_default();
    let argument = parts.next().unwrap_or_default().trim();
    let selected = COMMANDS
        .iter()
        .find(|command| command.name == typed)
        .or_else(|| {
            argument
                .is_empty()
                .then(|| state.slash_matches().get(state.slash_selection).copied())
                .flatten()
        });
    match selected {
        Some(command) => Action::ExecuteCommand {
            kind: command.kind,
            argument: argument.into(),
        },
        None => Action::CommandError(format!("Unknown command: /{typed}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::CommandKind;

    #[test]
    fn enter_resolves_exact_partial_and_unknown_commands_without_mutation() {
        let mut state = UiState::default();
        for (draft, kind, argument) in [
            (" /model", CommandKind::Model, ""),
            ("/ren", CommandKind::Rename, ""),
            ("/rename  New title ", CommandKind::Rename, "New title"),
        ] {
            state.orphan_draft = draft.into();
            assert_eq!(
                submit_action(&state),
                Action::ExecuteCommand {
                    kind,
                    argument: argument.into()
                }
            );
            assert_eq!(state.draft(), draft);
        }
        state.orphan_draft = "/unknown".into();
        assert!(matches!(submit_action(&state), Action::CommandError(_)));
        state.orphan_draft = "hello".into();
        assert_eq!(submit_action(&state), Action::Submit);
    }
}
