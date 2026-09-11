//! Focus ownership shared by every interactive region.
//!
//! Workspace regions choose their own visible expression: selection surface in
//! the rails and a caret in editors. Modal panels own the scope while open.

use crate::state::{Focus, UiState};

/// Modal panels own input while open; otherwise the workspace focus is the
/// single source of truth for every visible region.
pub(crate) fn workspace_focused(state: &UiState, focus: Focus) -> bool {
    state.panel.is_none() && state.focus == focus
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panels_suspend_workspace_focus() {
        for focus in [
            Focus::Sessions,
            Focus::SessionTitle,
            Focus::Composer,
            Focus::RightRail,
        ] {
            let mut state = UiState::default();
            state.focus = focus;
            assert!(workspace_focused(&state, focus));
            state.panel = Some(crate::state::Panel::Help);
            assert!(!workspace_focused(&state, focus));
        }
    }
}
