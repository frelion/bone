//! WorkspaceTarget ownership shared by every interactive region.
//!
//! Workspace regions choose their own visible expression: selection surface in
//! the rails and a caret in editors. Panels own the scope only after explicit keyboard activation.

use crate::state::{UiState, WorkspaceTarget};

/// Keyboard-activated panels own input; otherwise the workspace focus is the
/// single source of truth for every visible region.
pub(crate) fn workspace_focused(state: &UiState, focus: WorkspaceTarget) -> bool {
    state.keyboard == crate::state::KeyboardOwner::Workspace(focus)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panels_suspend_workspace_focus() {
        for focus in [
            WorkspaceTarget::Sessions,
            WorkspaceTarget::SessionTitle,
            WorkspaceTarget::Composer,
        ] {
            let mut state = UiState::default();
            state.set_workspace_target(focus);
            assert!(workspace_focused(&state, focus));
            state.overlay = Some(crate::state::Overlay::Help);
            state.enter_overlay();
            assert!(!workspace_focused(&state, focus));
        }
    }
}
