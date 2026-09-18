//! Pure keyboard mapping.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::{
    editor::{CursorMove, EditCommand},
    state::{Action, EditorTarget, KeyboardOwner, ModelScreen, Overlay, UiState, WorkspaceTarget},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct KeyGeometry {
    pub(super) composer_width: Option<u16>,
}

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

pub(super) fn key_action(key: KeyEvent, state: &UiState, geometry: KeyGeometry) -> Option<Action> {
    // Exit is global, including modal panels and the too-small fallback screen.
    if exact(&key, KeyCode::Char('d'), KeyModifiers::CONTROL) {
        return Some(Action::Quit);
    }

    // Enter chords are exact. This guard prevents a modified Enter from
    // activating a panel or a session after falling through another branch.
    if key.code == KeyCode::Enter && key.modifiers != KeyModifiers::NONE {
        return (!matches!(state.keyboard, KeyboardOwner::Overlay { .. })
            && state.workspace_target() == WorkspaceTarget::Composer
            && key.modifiers == KeyModifiers::SHIFT)
            .then(|| {
                edit(
                    EditorTarget::Composer,
                    EditCommand::Insert {
                        text: "\n".into(),
                        typing: false,
                    },
                )
            });
    }

    if exact(&key, KeyCode::F(6), KeyModifiers::NONE) && state.overlay.is_some() {
        return Some(Action::ToggleOverlayKeyboard);
    }

    // A visible panel owns an unmodified Enter as soon as the user expresses
    // keyboard intent. Merely opening it with the pointer still leaves typing
    // in the workspace.
    if exact(&key, KeyCode::Enter, KeyModifiers::NONE) && state.overlay.is_some() {
        return Some(Action::ActivatePanel);
    }

    if matches!(state.keyboard, KeyboardOwner::Overlay { .. })
        && let Some(panel) = &state.overlay
    {
        return overlay_action(key, panel);
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
            KeyCode::Char('c') if state.workspace_target() == WorkspaceTarget::Composer => {
                Some(edit(EditorTarget::Composer, EditCommand::Clear))
            }
            KeyCode::Char('z') => editor_target(state.workspace_target())
                .map(|target| edit(target, EditCommand::Undo)),
            KeyCode::Char('y') => editor_target(state.workspace_target())
                .map(|target| edit(target, EditCommand::Redo)),
            _ => None,
        };
    }

    if let Some(target) = editor_target(state.workspace_target())
        && key
            .modifiers
            .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT)
    {
        let cursor = match key.code {
            KeyCode::Left if key.modifiers.contains(KeyModifiers::ALT) => {
                Some(CursorMove::WordLeft)
            }
            KeyCode::Right if key.modifiers.contains(KeyModifiers::ALT) => {
                Some(CursorMove::WordRight)
            }
            KeyCode::Left => Some(CursorMove::Left),
            KeyCode::Right => Some(CursorMove::Right),
            KeyCode::Up if target == EditorTarget::Composer => geometry
                .composer_width
                .map(|width| CursorMove::Up { width }),
            KeyCode::Down if target == EditorTarget::Composer => geometry
                .composer_width
                .map(|width| CursorMove::Down { width }),
            KeyCode::Home if key.modifiers == KeyModifiers::SHIFT => Some(CursorMove::LineStart),
            KeyCode::End if key.modifiers == KeyModifiers::SHIFT => Some(CursorMove::LineEnd),
            _ => None,
        };
        if let Some(cursor) = cursor {
            return Some(edit(
                target,
                EditCommand::Move {
                    cursor,
                    select: key.modifiers.contains(KeyModifiers::SHIFT),
                },
            ));
        }
        if key.modifiers.contains(KeyModifiers::ALT) {
            return None;
        }
    }

    let slash_open = state.slash_palette_visible();
    let editor_target = editor_target(state.workspace_target());
    let action = match key.code {
        KeyCode::Esc if state.workspace_target() == WorkspaceTarget::SessionTitle => {
            Some(Action::CancelTitle)
        }
        KeyCode::Esc => Some(Action::Escape),
        KeyCode::Up if slash_open => Some(Action::SelectSlashPrevious),
        KeyCode::Down if slash_open => Some(Action::SelectSlashNext),
        KeyCode::Tab if slash_open => Some(Action::CompleteSlash),
        KeyCode::Up if state.workspace_target() == WorkspaceTarget::Sessions => {
            Some(Action::SelectPrevious)
        }
        KeyCode::Down if state.workspace_target() == WorkspaceTarget::Sessions => {
            Some(Action::SelectNext)
        }
        KeyCode::Enter if state.workspace_target() == WorkspaceTarget::Sessions => {
            Some(Action::OpenCandidate)
        }
        KeyCode::PageUp if editor_target.is_some() => Some(Action::ScrollUp(10)),
        KeyCode::PageDown if editor_target.is_some() => Some(Action::ScrollDown(10)),
        KeyCode::Enter if state.workspace_target() == WorkspaceTarget::SessionTitle => {
            Some(Action::CommitTitle)
        }
        KeyCode::Enter if state.workspace_target() == WorkspaceTarget::Composer => {
            Some(super::commands::submit_action(state))
        }
        _ => None,
    };
    action.or_else(|| editor_key_action(key, editor_target?, geometry))
}

fn editor_target(focus: WorkspaceTarget) -> Option<EditorTarget> {
    match focus {
        WorkspaceTarget::Composer => Some(EditorTarget::Composer),
        WorkspaceTarget::SessionTitle => Some(EditorTarget::SessionTitle),
        WorkspaceTarget::Sessions => None,
    }
}

fn editor_key_action(key: KeyEvent, target: EditorTarget, geometry: KeyGeometry) -> Option<Action> {
    let command = match key.code {
        KeyCode::Backspace => EditCommand::DeleteBefore,
        KeyCode::Delete => EditCommand::DeleteAfter,
        KeyCode::Left => return Some(move_editor(target, CursorMove::Left)),
        KeyCode::Right => return Some(move_editor(target, CursorMove::Right)),
        KeyCode::Home => return Some(move_editor(target, CursorMove::LineStart)),
        KeyCode::End => return Some(move_editor(target, CursorMove::LineEnd)),
        KeyCode::Up if target == EditorTarget::Composer => {
            return geometry
                .composer_width
                .map(|width| move_editor(target, CursorMove::Up { width }));
        }
        KeyCode::Down if target == EditorTarget::Composer => {
            return geometry
                .composer_width
                .map(|width| move_editor(target, CursorMove::Down { width }));
        }
        KeyCode::Char(value) => EditCommand::Insert {
            text: value.to_string(),
            typing: true,
        },
        _ => return None,
    };
    Some(edit(target, command))
}

fn edit(target: EditorTarget, command: EditCommand) -> Action {
    Action::Edit { target, command }
}

fn move_editor(target: EditorTarget, cursor: CursorMove) -> Action {
    edit(
        target,
        EditCommand::Move {
            cursor,
            select: false,
        },
    )
}

fn exact(key: &KeyEvent, code: KeyCode, modifiers: KeyModifiers) -> bool {
    key.code == code && key.modifiers == modifiers
}

fn overlay_action(key: KeyEvent, panel: &Overlay) -> Option<Action> {
    if matches!(
        panel,
        Overlay::Models(models) if matches!(models.screen, ModelScreen::Tab { .. })
    ) && exact(&key, KeyCode::Delete, KeyModifiers::NONE)
    {
        return Some(Action::DeleteModel);
    }
    if matches!(
        panel,
        Overlay::Models(models)
            if matches!(
                models.screen,
                ModelScreen::Setup(_) | ModelScreen::ModelForm(_)
            )
    ) {
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

    if matches!(
        panel,
        Overlay::Models(models) if matches!(models.screen, ModelScreen::Reasoning { .. })
    ) {
        return match key.code {
            KeyCode::Left => Some(Action::PanelPrevious),
            KeyCode::Right => Some(Action::PanelNext),
            KeyCode::Up => Some(Action::PanelPrevious),
            KeyCode::Down => Some(Action::PanelNext),
            _ => None,
        };
    }

    if let Overlay::Models(models) = panel {
        // The manual model editor mirrors the connection form's field editing,
        // but a model identifier is not a secret.
        if matches!(models.screen, ModelScreen::ModelInput { .. }) {
            if exact(&key, KeyCode::Char('u'), KeyModifiers::CONTROL) {
                return Some(Action::ModelClear);
            }
            if key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
            {
                return None;
            }
            return match key.code {
                KeyCode::Esc => Some(Action::Escape),
                KeyCode::Enter => Some(Action::ActivatePanel),
                KeyCode::Backspace => Some(Action::ModelBackspace),
                KeyCode::Char(ch) => Some(Action::ModelText(ch.to_string())),
                _ => None,
            };
        }
        // Deleting a connection answers yes or no, and nothing else.
        if matches!(models.screen, ModelScreen::ConfirmDelete { .. }) {
            return match key.code {
                KeyCode::Char('y') if key.modifiers == KeyModifiers::NONE => {
                    Some(Action::ConfirmDeleteConnection)
                }
                KeyCode::Char('n') if key.modifiers == KeyModifiers::NONE => Some(Action::Escape),
                KeyCode::Esc => Some(Action::Escape),
                _ => None,
            };
        }
        // Signing in has no rows to move through and no field to type into.
        // Enter retries a failed attempt - the same action the rendered row
        // carries, so the keyboard and the pointer agree.
        if matches!(models.screen, ModelScreen::Login { .. }) {
            return match key.code {
                KeyCode::Esc => Some(Action::Escape),
                KeyCode::Enter => Some(Action::ActivatePanel),
                _ => None,
            };
        }
    }

    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return None;
    }
    // The tab strip is the only screen with a horizontal axis: it moves between
    // connections, and `d`/`e` act on the connection the tab shows. Neither key
    // can be typed here, so a bare letter is unambiguous.
    let on_tab = matches!(
        panel,
        Overlay::Models(models) if matches!(models.screen, ModelScreen::Tab { .. })
    );
    match key.code {
        KeyCode::Esc => Some(Action::Escape),
        KeyCode::Left if on_tab => Some(Action::PreviousTab),
        KeyCode::Right if on_tab => Some(Action::NextTab),
        KeyCode::Char('d') if on_tab => Some(Action::DeleteConnection),
        KeyCode::Char('e') if on_tab => Some(Action::EditConnection),
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

    fn action_for_key(key: KeyEvent, state: &UiState) -> Option<Action> {
        key_action(
            key,
            state,
            KeyGeometry {
                composer_width: Some(37),
            },
        )
    }

    fn setup_panel() -> Overlay {
        let mut models = crate::state::ModelPanel::new(None);
        models.screen = ModelScreen::Setup(Box::new(crate::state::ConnectionForm::new(
            crate::state::ConnectionKind::OpenAiApi,
        )));
        Overlay::Models(models)
    }

    fn model_form_panel() -> Overlay {
        let mut profile = bone_app::Profile::chatgpt();
        profile.add_model("model-a").unwrap();
        let mut models = crate::state::ModelPanel::new(None);
        models.screen = ModelScreen::ModelForm(Box::new(crate::state::ModelForm::remove(
            profile, "model-a",
        )));
        Overlay::Models(models)
    }

    fn model_screen_panel(screen: ModelScreen) -> Overlay {
        let mut models = crate::state::ModelPanel::new(None);
        models.screen = screen;
        Overlay::Models(models)
    }

    #[test]
    fn spatial_focus_requires_exact_control() {
        let state = UiState::default();
        for code in [KeyCode::Left, KeyCode::Right, KeyCode::Up, KeyCode::Down] {
            assert!(action_for_key(key(code, KeyModifiers::CONTROL), &state).is_some());
            for extra in [
                KeyModifiers::SHIFT,
                KeyModifiers::ALT,
                KeyModifiers::SHIFT | KeyModifiers::ALT,
            ] {
                assert!(
                    action_for_key(key(code, KeyModifiers::CONTROL | extra), &state).is_none(),
                    "{code:?} with {extra:?} must not move focus"
                );
            }
        }
    }

    #[test]
    fn model_setup_rejects_control_modified_navigation() {
        let mut state = UiState::default();
        state.overlay = Some(setup_panel());
        state.enter_overlay();

        assert!(matches!(
            action_for_key(key(KeyCode::Char('u'), KeyModifiers::CONTROL), &state),
            Some(Action::SetupClear)
        ));
        assert!(
            action_for_key(
                key(
                    KeyCode::Char('u'),
                    KeyModifiers::CONTROL | KeyModifiers::SHIFT
                ),
                &state
            )
            .is_none()
        );
        for code in [KeyCode::Up, KeyCode::Down] {
            assert!(action_for_key(key(code, KeyModifiers::CONTROL), &state).is_none());
            assert!(
                action_for_key(
                    key(code, KeyModifiers::CONTROL | KeyModifiers::SHIFT),
                    &state
                )
                .is_none()
            );
        }
        assert!(matches!(
            action_for_key(key(KeyCode::Up, KeyModifiers::NONE), &state),
            Some(Action::PreviousField)
        ));
        assert!(matches!(
            action_for_key(key(KeyCode::Down, KeyModifiers::NONE), &state),
            Some(Action::NextField)
        ));
    }

    #[test]
    fn model_form_keeps_text_and_save_on_one_keyboard_route() {
        let mut state = UiState::default();
        state.overlay = Some(model_form_panel());
        state.enter_overlay();

        assert!(matches!(
            action_for_key(key(KeyCode::Char('q'), KeyModifiers::NONE), &state),
            Some(Action::SetupText(_))
        ));
        assert!(matches!(
            action_for_key(key(KeyCode::Enter, KeyModifiers::NONE), &state),
            Some(Action::ActivatePanel)
        ));
    }

    fn tab_panel() -> Overlay {
        model_screen_panel(ModelScreen::Tab { selected: 0 })
    }

    #[test]
    fn tab_arrows_move_between_connections_only_on_the_tab_screen() {
        let mut state = UiState::default();
        state.overlay = Some(tab_panel());
        state.enter_overlay();
        assert!(matches!(
            action_for_key(key(KeyCode::Left, KeyModifiers::NONE), &state),
            Some(Action::PreviousTab)
        ));
        assert!(matches!(
            action_for_key(key(KeyCode::Right, KeyModifiers::NONE), &state),
            Some(Action::NextTab)
        ));

        for screen in [
            ModelScreen::Kind { selected: 0 },
            ModelScreen::ModelInput {
                value: String::new(),
            },
            ModelScreen::ConfirmDelete { selected: 0 },
            ModelScreen::Reasoning {
                selected: 0,
                selection: bone_app::ModelSelection::new(bone_app::ProfileId::chatgpt(), "model")
                    .unwrap(),
            },
            ModelScreen::ModelForm(Box::new(crate::state::ModelForm::remove(
                bone_app::Profile::chatgpt(),
                "model",
            ))),
        ] {
            let mut state = UiState::default();
            state.overlay = Some(model_screen_panel(screen));
            state.enter_overlay();
            for code in [KeyCode::Left, KeyCode::Right] {
                assert!(
                    !matches!(
                        action_for_key(key(code, KeyModifiers::NONE), &state),
                        Some(Action::PreviousTab | Action::NextTab)
                    ),
                    "{code:?} must not move tabs off the strip"
                );
            }
        }
    }

    #[test]
    fn the_tab_screen_keeps_the_vertical_row_keys() {
        let mut state = UiState::default();
        state.overlay = Some(tab_panel());
        state.enter_overlay();
        assert!(matches!(
            action_for_key(key(KeyCode::Up, KeyModifiers::NONE), &state),
            Some(Action::PanelPrevious)
        ));
        assert!(matches!(
            action_for_key(key(KeyCode::Down, KeyModifiers::NONE), &state),
            Some(Action::PanelNext)
        ));
        assert!(matches!(
            action_for_key(key(KeyCode::Enter, KeyModifiers::NONE), &state),
            Some(Action::ActivatePanel)
        ));
        assert!(matches!(
            action_for_key(key(KeyCode::Esc, KeyModifiers::NONE), &state),
            Some(Action::Escape)
        ));
    }

    #[test]
    fn connection_editing_keys_stay_out_of_text_fields() {
        let mut state = UiState::default();
        state.overlay = Some(tab_panel());
        state.enter_overlay();
        assert!(matches!(
            action_for_key(key(KeyCode::Char('d'), KeyModifiers::NONE), &state),
            Some(Action::DeleteConnection)
        ));
        assert!(matches!(
            action_for_key(key(KeyCode::Char('e'), KeyModifiers::NONE), &state),
            Some(Action::EditConnection)
        ));
        for modifiers in [KeyModifiers::CONTROL, KeyModifiers::ALT] {
            for code in [KeyCode::Char('d'), KeyCode::Char('e')] {
                assert!(
                    !matches!(
                        action_for_key(key(code, modifiers), &state),
                        Some(Action::DeleteConnection | Action::EditConnection)
                    ),
                    "{code:?} with {modifiers:?} must not act on the connection"
                );
            }
        }
        // ctrl+d stays the global exit, never a connection shortcut.
        assert!(matches!(
            action_for_key(key(KeyCode::Char('d'), KeyModifiers::CONTROL), &state),
            Some(Action::Quit)
        ));

        state.overlay = Some(model_screen_panel(ModelScreen::ModelInput {
            value: String::new(),
        }));
        assert!(matches!(
            action_for_key(key(KeyCode::Char('d'), KeyModifiers::NONE), &state),
            Some(Action::ModelText(text)) if text == "d"
        ));
        assert!(matches!(
            action_for_key(key(KeyCode::Char('e'), KeyModifiers::NONE), &state),
            Some(Action::ModelText(text)) if text == "e"
        ));

        state.overlay = Some(setup_panel());
        assert!(matches!(
            action_for_key(key(KeyCode::Char('d'), KeyModifiers::NONE), &state),
            Some(Action::SetupText(text)) if text.as_str() == "d"
        ));
        assert!(matches!(
            action_for_key(key(KeyCode::Char('e'), KeyModifiers::NONE), &state),
            Some(Action::SetupText(text)) if text.as_str() == "e"
        ));
    }

    #[test]
    fn the_manual_model_editor_mirrors_the_form_keys() {
        let mut state = UiState::default();
        state.overlay = Some(model_screen_panel(ModelScreen::ModelInput {
            value: String::new(),
        }));
        state.enter_overlay();
        assert!(matches!(
            action_for_key(key(KeyCode::Esc, KeyModifiers::NONE), &state),
            Some(Action::Escape)
        ));
        assert!(matches!(
            action_for_key(key(KeyCode::Enter, KeyModifiers::NONE), &state),
            Some(Action::ActivatePanel)
        ));
        assert!(matches!(
            action_for_key(key(KeyCode::Backspace, KeyModifiers::NONE), &state),
            Some(Action::ModelBackspace)
        ));
        assert!(matches!(
            action_for_key(key(KeyCode::Char('u'), KeyModifiers::CONTROL), &state),
            Some(Action::ModelClear)
        ));
        assert!(
            action_for_key(
                key(
                    KeyCode::Char('u'),
                    KeyModifiers::CONTROL | KeyModifiers::SHIFT
                ),
                &state
            )
            .is_none()
        );
        assert!(matches!(
            action_for_key(key(KeyCode::Char('中'), KeyModifiers::NONE), &state),
            Some(Action::ModelText(text)) if text == "中"
        ));
        for code in [
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Tab,
        ] {
            assert!(
                action_for_key(key(code, KeyModifiers::NONE), &state).is_none(),
                "{code:?} does not edit the model identifier"
            );
        }
        let typed = action_for_key(key(KeyCode::Char('x'), KeyModifiers::NONE), &state).unwrap();
        assert!(!format!("{typed:?}").contains("SecretText"));
    }

    #[test]
    fn delete_confirmation_answers_yes_no_or_escape() {
        let mut state = UiState::default();
        state.overlay = Some(model_screen_panel(ModelScreen::ConfirmDelete {
            selected: 0,
        }));
        state.enter_overlay();
        assert!(matches!(
            action_for_key(key(KeyCode::Char('y'), KeyModifiers::NONE), &state),
            Some(Action::ConfirmDeleteConnection)
        ));
        assert!(matches!(
            action_for_key(key(KeyCode::Char('n'), KeyModifiers::NONE), &state),
            Some(Action::Escape)
        ));
        assert!(matches!(
            action_for_key(key(KeyCode::Esc, KeyModifiers::NONE), &state),
            Some(Action::Escape)
        ));
        // Enter keeps the shell's global meaning; the confirmation screen has
        // no activation, so the panel ignores it.
        assert!(matches!(
            action_for_key(key(KeyCode::Enter, KeyModifiers::NONE), &state),
            Some(Action::ActivatePanel)
        ));
        for code in [
            KeyCode::Char('d'),
            KeyCode::Char('e'),
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Backspace,
        ] {
            assert!(
                action_for_key(key(code, KeyModifiers::NONE), &state).is_none(),
                "{code:?} is not an answer to the confirmation"
            );
        }
        assert!(
            action_for_key(key(KeyCode::Char('y'), KeyModifiers::CONTROL), &state).is_none(),
            "a modified y is not a confirmation"
        );
    }

    #[test]
    fn the_login_screen_answers_escape_and_retry() {
        let mut state = UiState::default();
        state.overlay = Some(model_screen_panel(ModelScreen::Login {
            request: 1,
            state: bone_app::LoginState::Failed {
                message: "authorization expired".into(),
            },
        }));
        state.enter_overlay();
        assert!(matches!(
            action_for_key(key(KeyCode::Esc, KeyModifiers::NONE), &state),
            Some(Action::Escape)
        ));
        // Enter retries, matching the rendered "Retry sign-in" row.
        assert!(matches!(
            action_for_key(key(KeyCode::Enter, KeyModifiers::NONE), &state),
            Some(Action::ActivatePanel)
        ));
        for code in [
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Char('d'),
            KeyCode::Char('e'),
            KeyCode::Char('y'),
            KeyCode::Char('n'),
            KeyCode::Char('x'),
            KeyCode::Backspace,
        ] {
            assert!(
                action_for_key(key(code, KeyModifiers::NONE), &state).is_none(),
                "{code:?} is not a sign-in key"
            );
        }
        assert!(
            action_for_key(key(KeyCode::Char('u'), KeyModifiers::CONTROL), &state).is_none(),
            "the login screen has no field to clear"
        );
    }

    #[test]
    fn the_kind_picker_keeps_one_vertical_route_to_every_connection_kind() {
        let mut state = UiState::default();
        state.overlay = Some(model_screen_panel(ModelScreen::Kind { selected: 0 }));
        state.enter_overlay();
        assert!(matches!(
            action_for_key(key(KeyCode::Up, KeyModifiers::NONE), &state),
            Some(Action::PanelPrevious)
        ));
        assert!(matches!(
            action_for_key(key(KeyCode::Down, KeyModifiers::NONE), &state),
            Some(Action::PanelNext)
        ));
        assert!(matches!(
            action_for_key(key(KeyCode::Enter, KeyModifiers::NONE), &state),
            Some(Action::ActivatePanel)
        ));
        assert!(matches!(
            action_for_key(key(KeyCode::Esc, KeyModifiers::NONE), &state),
            Some(Action::Escape)
        ));
        for code in [KeyCode::Char('d'), KeyCode::Char('e')] {
            assert!(
                !matches!(
                    action_for_key(key(code, KeyModifiers::NONE), &state),
                    Some(Action::DeleteConnection | Action::EditConnection)
                ),
                "{code:?} is inert while choosing a kind"
            );
        }
    }

    #[test]
    fn documented_input_chords_remain_exact_without_aliases() {
        let state = UiState::default();
        assert!(matches!(
            action_for_key(key(KeyCode::Enter, KeyModifiers::NONE), &state),
            Some(Action::Submit)
        ));
        assert!(matches!(
            action_for_key(key(KeyCode::Enter, KeyModifiers::SHIFT), &state),
            Some(Action::Edit {
                target: EditorTarget::Composer,
                command: EditCommand::Insert { ref text, typing: false }
            }) if text == "\n"
        ));
        assert!(matches!(
            action_for_key(key(KeyCode::Char('c'), KeyModifiers::CONTROL), &state),
            Some(Action::Edit {
                target: EditorTarget::Composer,
                command: EditCommand::Clear
            })
        ));
        assert!(matches!(
            action_for_key(key(KeyCode::Char('d'), KeyModifiers::CONTROL), &state),
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
            assert!(action_for_key(key, &state).is_none());
        }
    }

    #[test]
    fn an_unmatched_slash_query_still_owns_menu_navigation() {
        let mut state = UiState::default();
        state.orphan_draft = "/does-not-exist".into();
        assert!(state.slash_palette_visible());
        assert!(state.slash_matches().is_empty());
        assert!(matches!(
            action_for_key(key(KeyCode::Up, KeyModifiers::NONE), &state),
            Some(Action::SelectSlashPrevious)
        ));
        assert!(matches!(
            action_for_key(key(KeyCode::Down, KeyModifiers::NONE), &state),
            Some(Action::SelectSlashNext)
        ));
    }

    #[test]
    fn geometry_bound_keys_do_not_invent_measurements() {
        let state = UiState::default();
        let measured = KeyGeometry {
            composer_width: Some(51),
        };
        let unavailable = KeyGeometry {
            composer_width: None,
        };

        assert!(matches!(
            key_action(key(KeyCode::Down, KeyModifiers::NONE), &state, measured),
            Some(Action::Edit {
                target: EditorTarget::Composer,
                command: EditCommand::Move {
                    cursor: CursorMove::Down { width: 51 },
                    select: false,
                },
            })
        ));
        assert!(key_action(key(KeyCode::Down, KeyModifiers::NONE), &state, unavailable,).is_none());
        assert!(
            key_action(key(KeyCode::Right, KeyModifiers::CONTROL), &state, measured,).is_some()
        );
    }
}
