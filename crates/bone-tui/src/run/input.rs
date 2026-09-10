use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};

use crate::{
    layout::{HitTarget, LayoutPlan},
    state::{Action, Focus, MainView, UiEvent, UiState},
};

pub(super) fn terminal_event(
    event: Event,
    layout: Option<&LayoutPlan>,
    state: &UiState,
) -> UiEvent {
    match event {
        Event::Key(key) if key.kind == KeyEventKind::Press => {
            UiEvent::Action(key_action(key, layout, state))
        }
        Event::Mouse(mouse) => {
            let target = layout.and_then(|plan| plan.hit(mouse.column, mouse.row));
            let action = match mouse.kind {
                MouseEventKind::ScrollUp => Action::ScrollUp(3),
                MouseEventKind::ScrollDown => Action::ScrollDown(3),
                MouseEventKind::Down(MouseButton::Left) => {
                    target.map_or(Action::Noop, |target| hit_action(target, state))
                }
                _ => Action::Noop,
            };
            UiEvent::Action(action)
        }
        Event::Resize(_, _) => UiEvent::Resized,
        Event::Paste(text) => {
            // Paste is text only. It never crosses the local action boundary.
            UiEvent::Action(Action::Paste(text))
        }
        _ => UiEvent::Tick,
    }
}

fn key_action(key: KeyEvent, layout: Option<&LayoutPlan>, state: &UiState) -> Action {
    if key.code == KeyCode::Enter
        && (key.modifiers.contains(KeyModifiers::SHIFT)
            || key.modifiers.contains(KeyModifiers::ALT))
    {
        return Action::InsertNewline;
    }
    match (key.code, key.modifiers) {
        (KeyCode::Char('q'), KeyModifiers::CONTROL) => Action::Quit,
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => Action::Quit,
        (KeyCode::Char('n'), KeyModifiers::CONTROL) => Action::NewSession,
        (KeyCode::Tab, KeyModifiers::SHIFT) | (KeyCode::BackTab, _) => Action::FocusPrevious,
        (KeyCode::Tab, _) => Action::FocusNext,
        (KeyCode::Esc, _) => Action::Escape,
        (KeyCode::Left | KeyCode::Up, _) if visible_control_region_is_active(state) => {
            move_visible_control(layout, state, -1)
        }
        (KeyCode::Right | KeyCode::Down, _) if visible_control_region_is_active(state) => {
            move_visible_control(layout, state, 1)
        }
        (KeyCode::Up, _) if state.main == MainView::Attention && state.detail.is_none() => {
            Action::MoveAttention(-1)
        }
        (KeyCode::Down, _) if state.main == MainView::Attention && state.detail.is_none() => {
            Action::MoveAttention(1)
        }
        (KeyCode::Left, _)
            if state
                .detail
                .as_ref()
                .is_some_and(|detail| detail.kind == crate::state::DetailKind::Decision) =>
        {
            Action::MoveWriteResolution(-1)
        }
        (KeyCode::Right, _)
            if state
                .detail
                .as_ref()
                .is_some_and(|detail| detail.kind == crate::state::DetailKind::Decision) =>
        {
            Action::MoveWriteResolution(1)
        }
        (KeyCode::Left, _) if workspace_file_is_active(state) => {
            Action::MoveWorkspaceFileControl(-1)
        }
        (KeyCode::Right, _) if workspace_file_is_active(state) => {
            Action::MoveWorkspaceFileControl(1)
        }
        (KeyCode::Left, _)
            if matches!(state.focus, Focus::Global | Focus::Detail | Focus::Actions) =>
        {
            Action::MoveFocusedControl(-1)
        }
        (KeyCode::Right, _)
            if matches!(state.focus, Focus::Global | Focus::Detail | Focus::Actions) =>
        {
            Action::MoveFocusedControl(1)
        }
        (KeyCode::Up, _) if changes_list_is_active(state) => Action::MoveWorkspaceChange(-1),
        (KeyCode::Down, _) if changes_list_is_active(state) => Action::MoveWorkspaceChange(1),
        (KeyCode::PageUp, _) if changes_list_is_active(state) => Action::MoveWorkspaceChange(-10),
        (KeyCode::PageDown, _) if changes_list_is_active(state) => Action::MoveWorkspaceChange(10),
        (KeyCode::Up, _) if evidence_list_is_active(state) => Action::MoveEvidence(-1),
        (KeyCode::Down, _) if evidence_list_is_active(state) => Action::MoveEvidence(1),
        (KeyCode::PageUp, _) if evidence_list_is_active(state) => Action::MoveEvidence(-10),
        (KeyCode::PageDown, _) if evidence_list_is_active(state) => Action::MoveEvidence(10),
        (KeyCode::Up, _) if state.main == MainView::Sessions => Action::SelectPrevious,
        (KeyCode::Down, _) if state.main == MainView::Sessions => Action::SelectNext,
        (KeyCode::Up, _) if state.focus == Focus::Rail => Action::SelectPrevious,
        (KeyCode::Down, _) if state.focus == Focus::Rail => Action::SelectNext,
        (KeyCode::Up, _) => Action::ScrollUp(1),
        (KeyCode::Down, _) => Action::ScrollDown(1),
        (KeyCode::PageUp, _) => Action::ScrollUp(10),
        (KeyCode::PageDown, _) => Action::ScrollDown(10),
        (KeyCode::Enter, _) if visible_control_region_is_active(state) => {
            activate_visible_control(layout, state)
        }
        (KeyCode::Enter, _)
            if matches!(state.focus, Focus::Global | Focus::Actions)
                || (state.detail.is_some() && state.focus == Focus::Detail)
                || (state.main == MainView::Attention && state.detail.is_none()) =>
        {
            Action::Activate
        }
        (KeyCode::Enter, _) => Action::Submit,
        (KeyCode::Backspace, _) => Action::Backspace,
        (KeyCode::Char(value), KeyModifiers::NONE | KeyModifiers::SHIFT) => Action::Input(value),
        _ => Action::Noop,
    }
}

fn visible_control_region_is_active(state: &UiState) -> bool {
    (state.main == MainView::Settings && state.detail.is_none() && state.focus == Focus::Timeline)
        || (state.focus == Focus::Detail
            && state.detail.as_ref().is_some_and(|detail| {
                matches!(
                    detail.kind,
                    crate::state::DetailKind::Changes
                        | crate::state::DetailKind::Artifacts
                        | crate::state::DetailKind::Acceptance
                )
            }))
}

fn visible_control_targets(layout: Option<&LayoutPlan>, state: &UiState) -> Vec<HitTarget> {
    let Some(layout) = layout else {
        return Vec::new();
    };
    layout
        .hit_regions
        .iter()
        .map(|region| region.target)
        .filter(|target| match target {
            HitTarget::SettingsProfile(_)
            | HitTarget::ConfigureWorker
            | HitTarget::ConfigureCoordinator
            | HitTarget::Login
            | HitTarget::Logout => state.main == MainView::Settings && state.detail.is_none(),
            HitTarget::DetailWork
            | HitTarget::DetailChanges
            | HitTarget::DetailContext
            | HitTarget::DetailArtifacts
            | HitTarget::DetailRecords
            | HitTarget::DetailAcceptance
            | HitTarget::CloseDetail => state.detail.as_ref().is_some_and(|detail| {
                matches!(
                    detail.kind,
                    crate::state::DetailKind::Changes
                        | crate::state::DetailKind::Artifacts
                        | crate::state::DetailKind::Acceptance
                )
            }),
            HitTarget::WorkspaceChange(_)
            | HitTarget::OlderWorkspaceChanges
            | HitTarget::NewerWorkspaceChanges
            | HitTarget::RefreshWorkspaceChanges
            | HitTarget::PreviousWorkspaceFile
            | HitTarget::MoreWorkspaceFile
            | HitTarget::CloseWorkspaceFile => state
                .detail
                .as_ref()
                .is_some_and(|detail| detail.kind == crate::state::DetailKind::Changes),
            HitTarget::Evidence(_)
            | HitTarget::OlderEvidence
            | HitTarget::NewerEvidence
            | HitTarget::RefreshArtifact
            | HitTarget::PreviousEvidenceSource
            | HitTarget::MoreEvidenceSource
            | HitTarget::CloseEvidenceSource => state
                .detail
                .as_ref()
                .is_some_and(|detail| detail.kind == crate::state::DetailKind::Artifacts),
            HitTarget::Accept
            | HitTarget::PartiallyAccept
            | HitTarget::AcceptWithRisk
            | HitTarget::Reject
            | HitTarget::OlderResults
            | HitTarget::NewerResults
            | HitTarget::OlderAcceptances
            | HitTarget::NewerAcceptances => state
                .detail
                .as_ref()
                .is_some_and(|detail| detail.kind == crate::state::DetailKind::Acceptance),
            _ => false,
        })
        .collect()
}

fn selected_visible_control(targets: &[HitTarget], state: &UiState) -> usize {
    state
        .focused_control
        .and_then(|selected| targets.iter().position(|target| *target == selected))
        .unwrap_or_else(|| {
            targets
                .iter()
                .position(|target| match target {
                    HitTarget::SettingsProfile(index) => *index == state.settings_profile,
                    HitTarget::DetailWork => state.detail_tab_selection == 0,
                    HitTarget::DetailChanges => state.detail_tab_selection == 1,
                    HitTarget::DetailContext => state.detail_tab_selection == 2,
                    HitTarget::DetailArtifacts => state.detail_tab_selection == 3,
                    HitTarget::DetailRecords => state.detail_tab_selection == 4,
                    HitTarget::DetailAcceptance => state.detail_tab_selection == 5,
                    _ => false,
                })
                .unwrap_or(0)
        })
}

fn move_visible_control(layout: Option<&LayoutPlan>, state: &UiState, delta: isize) -> Action {
    let targets = visible_control_targets(layout, state);
    if targets.is_empty() {
        return Action::Noop;
    }
    let current = selected_visible_control(&targets, state);
    let next = current
        .saturating_add_signed(delta)
        .min(targets.len().saturating_sub(1));
    Action::FocusVisibleControl(targets[next])
}

fn activate_visible_control(layout: Option<&LayoutPlan>, state: &UiState) -> Action {
    let targets = visible_control_targets(layout, state);
    if targets.is_empty() {
        return Action::Noop;
    }
    hit_action(targets[selected_visible_control(&targets, state)], state)
}

fn changes_list_is_active(state: &UiState) -> bool {
    state.focus == Focus::Detail
        && state
            .detail
            .as_ref()
            .is_some_and(|detail| detail.kind == crate::state::DetailKind::Changes)
        && state.workspace_changes.file.is_none()
        && !state.workspace_changes.file_loading
}

fn workspace_file_is_active(state: &UiState) -> bool {
    state.focus == Focus::Detail
        && state
            .detail
            .as_ref()
            .is_some_and(|detail| detail.kind == crate::state::DetailKind::Changes)
        && state.workspace_changes.file.is_some()
}

fn evidence_list_is_active(state: &UiState) -> bool {
    state.focus == Focus::Detail
        && state
            .detail
            .as_ref()
            .is_some_and(|detail| detail.kind == crate::state::DetailKind::Artifacts)
        && state.artifact.source.is_none()
        && !state.artifact.source_loading
}

fn hit_action(target: HitTarget, state: &UiState) -> Action {
    match target {
        HitTarget::Workbench => Action::Open(MainView::Workbench),
        HitTarget::Sessions => Action::Open(MainView::Sessions),
        HitTarget::Attention => Action::Open(MainView::Attention),
        HitTarget::Settings => Action::Open(MainView::Settings),
        HitTarget::NewSession => Action::NewSession,
        HitTarget::Session(index) => state
            .sessions
            .get(index)
            .map_or(Action::Activate, |session| {
                Action::SelectSession(session.id)
            }),
        HitTarget::RenameSession => state
            .selected
            .map_or(Action::Noop, Action::BeginRenameSession),
        HitTarget::ArchiveSession => state
            .selected
            .and_then(|id| {
                state
                    .sessions
                    .iter()
                    .find(|session| session.id == id)
                    .map(|session| Action::SetSessionArchived {
                        session: id,
                        archived: !session.archived,
                    })
            })
            .unwrap_or(Action::Noop),
        HitTarget::CloseDetail => Action::CloseDetail,
        HitTarget::Composer => Action::Focus(Focus::Composer),
        HitTarget::Submit => Action::SubmitFromActionBar,
        HitTarget::Stop => Action::Stop,
        HitTarget::Quit => Action::Quit,
        HitTarget::DialogConfirm => Action::Activate,
        HitTarget::Acceptance => Action::OpenDetail(crate::state::DetailState {
            kind: crate::state::DetailKind::Work,
            title: "工作详情".into(),
        }),
        HitTarget::DetailWork => Action::OpenDetail(crate::state::DetailState {
            kind: crate::state::DetailKind::Work,
            title: "工作详情".into(),
        }),
        HitTarget::DetailChanges => Action::OpenDetail(crate::state::DetailState {
            kind: crate::state::DetailKind::Changes,
            title: "工作区变更".into(),
        }),
        HitTarget::DetailContext => Action::OpenDetail(crate::state::DetailState {
            kind: crate::state::DetailKind::Context,
            title: "会话上下文".into(),
        }),
        HitTarget::DetailArtifacts => Action::OpenDetail(crate::state::DetailState {
            kind: crate::state::DetailKind::Artifacts,
            title: "产物与证据".into(),
        }),
        HitTarget::DetailRecords => Action::OpenDetail(crate::state::DetailState {
            kind: crate::state::DetailKind::Records,
            title: "事实记录".into(),
        }),
        HitTarget::DetailAcceptance => Action::OpenDetail(crate::state::DetailState {
            kind: crate::state::DetailKind::Acceptance,
            title: "结果与验收".into(),
        }),
        HitTarget::Accept => acceptance_action(state, bone_app::AcceptanceDecision::Accepted),
        HitTarget::PartiallyAccept => {
            acceptance_action(state, bone_app::AcceptanceDecision::PartiallyAccepted)
        }
        HitTarget::AcceptWithRisk => {
            acceptance_action(state, bone_app::AcceptanceDecision::AcceptedWithRisk)
        }
        HitTarget::Reject => acceptance_action(state, bone_app::AcceptanceDecision::Rejected),
        HitTarget::OlderResults => Action::LoadOlderResults,
        HitTarget::NewerResults => Action::LoadNewerResults,
        HitTarget::OlderAcceptances => Action::LoadOlderAcceptances,
        HitTarget::NewerAcceptances => Action::LoadNewerAcceptances,
        HitTarget::SettingsProfile(index) => Action::SelectSettingsProfile(index),
        HitTarget::ConfigureWorker => Action::ConfigureModel(crate::state::ModelRole::Worker),
        HitTarget::ConfigureCoordinator => {
            Action::ConfigureModel(crate::state::ModelRole::Coordinator)
        }
        HitTarget::Login => Action::ConfigureCredential,
        HitTarget::Logout => Action::Logout,
        HitTarget::AttentionItem(index) => Action::OpenAttention(index),
        HitTarget::WriteApplied => Action::BeginWriteResolution(bone_app::ExternalEffect::Applied),
        HitTarget::WriteNotApplied => Action::BeginWriteResolution(bone_app::ExternalEffect::None),
        HitTarget::WorkspaceChange(index) => Action::OpenWorkspaceChange(index),
        HitTarget::OlderWorkspaceChanges => Action::LoadOlderWorkspaceChanges,
        HitTarget::NewerWorkspaceChanges => Action::LoadNewerWorkspaceChanges,
        HitTarget::RefreshWorkspaceChanges => Action::RefreshWorkspaceChanges,
        HitTarget::PreviousWorkspaceFile => Action::LoadPreviousWorkspaceFile,
        HitTarget::MoreWorkspaceFile => Action::LoadMoreWorkspaceFile,
        HitTarget::CloseWorkspaceFile => Action::CloseWorkspaceFile,
        HitTarget::Evidence(index) => Action::OpenEvidence(index),
        HitTarget::OlderEvidence => Action::LoadOlderEvidence,
        HitTarget::NewerEvidence => Action::LoadNewerEvidence,
        HitTarget::RefreshArtifact => Action::RefreshArtifact,
        HitTarget::MoreEvidenceSource => Action::LoadMoreEvidenceSource,
        HitTarget::PreviousEvidenceSource => Action::LoadPreviousEvidenceSource,
        HitTarget::CloseEvidenceSource => Action::CloseEvidenceSource,
    }
}

fn acceptance_action(state: &UiState, decision: bone_app::AcceptanceDecision) -> Action {
    state
        .acceptance_target()
        .map_or(Action::Noop, |result| Action::BeginAcceptance {
            decision,
            result,
        })
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyModifiers, MouseEvent};
    use ratatui::{Terminal, backend::TestBackend};

    use super::*;
    use crate::state::{Dialog, update};

    fn render_plan(state: &UiState, width: u16) -> LayoutPlan {
        let backend = TestBackend::new(width, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut plan = None;
        terminal
            .draw(|frame| plan = Some(crate::view::render(frame, state)))
            .unwrap();
        plan.unwrap()
    }

    fn assert_rendered_controls_have_identical_keyboard_and_mouse_actions(
        state: &mut UiState,
        width: u16,
    ) -> Vec<HitTarget> {
        let plan = render_plan(state, width);
        let targets = visible_control_targets(Some(&plan), state);
        assert!(!targets.is_empty(), "no controls at width {width}");
        for target in targets.iter().copied() {
            state.focused_control = Some(target);
            let keyboard = key_action(
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                Some(&plan),
                state,
            );
            let mouse = hit_action(target, state);
            assert_eq!(
                format!("{keyboard:?}"),
                format!("{mouse:?}"),
                "keyboard/mouse action diverged for {target:?} at width {width}"
            );
        }
        state.focused_control = Some(targets[0]);
        if targets.len() > 1 {
            assert!(matches!(
                key_action(
                    KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
                    Some(&plan),
                    state,
                ),
                Action::FocusVisibleControl(target) if target == targets[1]
            ));
        }
        assert!(matches!(
            key_action(
                KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
                Some(&plan),
                state,
            ),
            Action::FocusNext
        ));
        assert!(matches!(
            key_action(
                KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT),
                Some(&plan),
                state,
            ),
            Action::FocusPrevious
        ));
        targets
    }

    #[test]
    fn incidental_mouse_events_cannot_confirm_a_dialog() {
        let mut state = UiState {
            dialog: Some(Dialog::ConfirmQuit),
            ..UiState::default()
        };
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut plan = None;
        terminal
            .draw(|frame| plan = Some(crate::view::render(frame, &state)))
            .unwrap();
        let moved = Event::Mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            column: 20,
            row: 10,
            modifiers: KeyModifiers::NONE,
        });
        let event = terminal_event(moved, plan.as_ref(), &state);
        assert!(update(&mut state, event).is_empty());
        assert!(matches!(state.dialog, Some(Dialog::ConfirmQuit)));
        assert!(!state.quitting);
    }

    #[test]
    fn arbitrary_keys_never_confirm_a_dialog() {
        let state = UiState {
            dialog: Some(Dialog::ConfirmQuit),
            ..UiState::default()
        };
        for code in [KeyCode::Left, KeyCode::Delete, KeyCode::Home, KeyCode::F(1)] {
            assert!(matches!(
                key_action(KeyEvent::new(code, KeyModifiers::NONE), None, &state),
                Action::Noop
            ));
        }
    }

    #[test]
    fn arrows_and_enter_drive_focused_visible_controls() {
        let mut state = UiState {
            focus: Focus::Global,
            ..UiState::default()
        };
        assert!(matches!(
            key_action(
                KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
                None,
                &state
            ),
            Action::MoveFocusedControl(1)
        ));
        assert!(matches!(
            key_action(
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                None,
                &state
            ),
            Action::Activate
        ));

        state.focus = Focus::Detail;
        state.detail = Some(crate::state::DetailState {
            kind: crate::state::DetailKind::Context,
            title: "上下文".into(),
        });
        assert!(matches!(
            key_action(
                KeyEvent::new(KeyCode::Left, KeyModifiers::NONE),
                None,
                &state
            ),
            Action::MoveFocusedControl(-1)
        ));
        assert!(matches!(
            key_action(
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                None,
                &state
            ),
            Action::Activate
        ));

        state.focus = Focus::Actions;
        assert!(matches!(
            key_action(
                KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
                None,
                &state
            ),
            Action::MoveFocusedControl(1)
        ));
        assert!(matches!(
            key_action(
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                None,
                &state
            ),
            Action::Activate
        ));
    }

    #[test]
    fn workspace_file_controls_use_the_same_keyboard_and_mouse_actions() {
        let mut state = UiState {
            focus: Focus::Detail,
            detail: Some(crate::state::DetailState {
                kind: crate::state::DetailKind::Changes,
                title: "工作区变更".into(),
            }),
            ..UiState::default()
        };
        state.workspace_changes.file = Some(bone_app::WorkspaceFilePage {
            baseline: bone_app::WorkspaceBaseline::NotGit,
            path: "large.txt".into(),
            source: bone_app::WorkspaceFileSource::WorkingTree,
            media: bone_app::WorkspaceFileMedia::Text,
            text: Some("window".into()),
            offset: 0,
            bytes_read: 6,
            total_bytes: Some(6),
            next_cursor: None,
        });

        let plan = render_plan(&state, 120);
        state.focused_control = Some(HitTarget::CloseWorkspaceFile);
        assert!(matches!(
            key_action(
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                Some(&plan),
                &state
            ),
            Action::CloseWorkspaceFile
        ));
        assert!(matches!(
            hit_action(HitTarget::PreviousWorkspaceFile, &state),
            Action::LoadPreviousWorkspaceFile
        ));
        assert!(matches!(
            hit_action(HitTarget::MoreWorkspaceFile, &state),
            Action::LoadMoreWorkspaceFile
        ));
        assert!(matches!(
            hit_action(HitTarget::CloseWorkspaceFile, &state),
            Action::CloseWorkspaceFile
        ));
    }

    #[test]
    fn settings_visible_controls_are_keyboard_reachable_at_every_breakpoint() {
        let session = bone_app::SessionId::new();
        let mut state = UiState {
            main: MainView::Settings,
            focus: Focus::Timeline,
            selected: Some(session),
            settings: Some(crate::state::SettingsData {
                session,
                generation: 1,
                query: 1,
                resolved: bone_app::ResolvedConfig {
                    desired: Err(bone_app::ConfigProblem::NeedsModel),
                    running: None,
                },
                profiles: vec![bone_app::Profile::chatgpt()],
            }),
            ..UiState::default()
        };
        for width in [40, 80, 120, 160] {
            let targets = assert_rendered_controls_have_identical_keyboard_and_mouse_actions(
                &mut state, width,
            );
            for expected in [
                HitTarget::SettingsProfile(0),
                HitTarget::ConfigureWorker,
                HitTarget::ConfigureCoordinator,
                HitTarget::Login,
                HitTarget::Logout,
            ] {
                assert!(
                    targets.contains(&expected),
                    "missing {expected:?} at {width}"
                );
            }
        }
    }

    fn artifact_state_with_source(source_open: bool) -> UiState {
        let session = bone_app::SessionId::new();
        let result = bone_app::ResultRef {
            session,
            job: bone_app::JobRef {
                runtime: bone_app::RuntimeId::new(),
                id: 1,
            },
            version: bone_app::SessionSeq(3),
        };
        let source = bone_app::EvidenceRef { session, record: 7 };
        let mut state = UiState {
            selected: Some(session),
            focus: Focus::Detail,
            detail: Some(crate::state::DetailState {
                kind: crate::state::DetailKind::Artifacts,
                title: "产物与证据".into(),
            }),
            detail_tab_selection: 3,
            ..UiState::default()
        };
        state.artifact.artifact = Some(bone_app::ResultArtifact {
            result,
            outcome: bone_app::OutcomeKind::Completed,
            summary: "result".into(),
            remaining: Vec::new(),
            evidence_count: 1,
        });
        state.artifact.evidence = Some(bone_app::EvidencePage {
            result,
            items: vec![bone_app::EvidenceSummary {
                source,
                availability: bone_app::EvidenceAvailability::Available {
                    kind: bone_app::EvidenceSourceKind::ToolResult,
                    title: "tool output".into(),
                },
            }],
            next_cursor: None,
            projection_pending: false,
        });
        state.artifact.evidence_pages.back.push(None);
        if source_open {
            state.artifact.source = Some(crate::state::EvidenceReaderUi {
                result,
                source,
                availability: bone_app::EvidenceAvailability::Available {
                    kind: bone_app::EvidenceSourceKind::ToolResult,
                    title: "tool output".into(),
                },
                text: "window".into(),
                window_offset: 1024,
                next_offset: Some(2048),
                total_bytes: Some(3072),
                projection_pending: false,
                frontend_truncated: false,
                layout: Default::default(),
            });
        }
        state
    }

    #[test]
    fn artifact_list_and_reader_controls_are_keyboard_reachable_at_every_breakpoint() {
        for width in [40, 80, 120, 160] {
            let mut list = artifact_state_with_source(false);
            let targets = assert_rendered_controls_have_identical_keyboard_and_mouse_actions(
                &mut list, width,
            );
            for expected in [
                HitTarget::Evidence(0),
                HitTarget::RefreshArtifact,
                HitTarget::NewerEvidence,
            ] {
                assert!(
                    targets.contains(&expected),
                    "missing {expected:?} at {width}"
                );
            }

            let mut reader = artifact_state_with_source(true);
            let targets = assert_rendered_controls_have_identical_keyboard_and_mouse_actions(
                &mut reader,
                width,
            );
            for expected in [
                HitTarget::CloseEvidenceSource,
                HitTarget::PreviousEvidenceSource,
                HitTarget::MoreEvidenceSource,
            ] {
                assert!(
                    targets.contains(&expected),
                    "missing {expected:?} at {width}"
                );
            }
        }
    }

    fn acceptance_state() -> UiState {
        let info = bone_app::SessionInfo {
            id: bone_app::SessionId::new(),
            workspace: bone_app::WorkspaceId::new(),
            title: "acceptance".into(),
            archived: false,
        };
        let result = bone_app::ResultRef {
            session: info.id,
            job: bone_app::JobRef {
                runtime: bone_app::RuntimeId::new(),
                id: 1,
            },
            version: bone_app::SessionSeq(3),
        };
        let mut state = UiState {
            sessions: vec![info.clone()],
            selected: Some(info.id),
            focus: Focus::Detail,
            detail: Some(crate::state::DetailState {
                kind: crate::state::DetailKind::Acceptance,
                title: "结果与验收".into(),
            }),
            detail_tab_selection: 5,
            ..UiState::default()
        };
        state
            .session_ui
            .insert(info.id, crate::state::SessionUi::new(info.clone(), 1));
        state.results.insert(
            info.id,
            bone_app::ResultPage {
                items: vec![bone_app::ResultSummary {
                    result,
                    outcome: bone_app::OutcomeKind::Completed,
                    summary: "done".into(),
                    remaining: Vec::new(),
                }],
                older_cursor: None,
                snapshot_through: result.version,
                projection_pending: true,
            },
        );
        state
            .session_ui
            .get_mut(&info.id)
            .unwrap()
            .result_pages
            .back
            .push(None);
        state.acceptance_windows_stale.insert(result);
        state
            .acceptance_pages
            .entry(result)
            .or_default()
            .back
            .push(None);
        state
    }

    #[test]
    fn acceptance_controls_are_reachable_and_old_pages_cannot_activate_a_decision() {
        for width in [40, 80, 120, 160] {
            let mut state = acceptance_state();
            let targets = assert_rendered_controls_have_identical_keyboard_and_mouse_actions(
                &mut state, width,
            );
            for expected in [
                HitTarget::Accept,
                HitTarget::PartiallyAccept,
                HitTarget::AcceptWithRisk,
                HitTarget::Reject,
                HitTarget::NewerResults,
                HitTarget::OlderResults,
                HitTarget::NewerAcceptances,
                HitTarget::OlderAcceptances,
            ] {
                assert!(
                    targets.contains(&expected),
                    "missing {expected:?} at {width}"
                );
            }

            state.detail_scroll = 1;
            state.focused_control = Some(HitTarget::Accept);
            let plan = render_plan(&state, width);
            assert!(!visible_control_targets(Some(&plan), &state).contains(&HitTarget::Accept));
            assert!(!matches!(
                key_action(
                    KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                    Some(&plan),
                    &state,
                ),
                Action::BeginAcceptance { .. }
            ));
        }
    }

    #[tokio::test]
    async fn workspace_change_list_and_file_controls_are_reachable_at_every_breakpoint() {
        let temporary = tempfile::tempdir().unwrap();
        let workspace_root = temporary.path().join("workspace");
        std::fs::create_dir(&workspace_root).unwrap();
        let status = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&workspace_root)
            .status()
            .unwrap();
        assert!(status.success());
        std::fs::write(workspace_root.join("a.txt"), "a").unwrap();
        std::fs::write(workspace_root.join("b.txt"), "b").unwrap();
        std::fs::write(workspace_root.join("large.txt"), "中文正文".repeat(1024)).unwrap();

        let app = bone_app::App::open(bone_app::AppOptions::new(temporary.path().join("data")))
            .await
            .unwrap();
        let workspace = app.open_workspace(&workspace_root).await.unwrap();
        let page = app.workspace_changes(workspace.id, None, 1).await.unwrap();
        assert!(page.next_cursor.is_some());
        let file = app
            .workspace_file_page(
                workspace.id,
                "large.txt",
                bone_app::WorkspaceFileSource::WorkingTree,
                None,
                64,
            )
            .await
            .unwrap();
        assert!(file.next_cursor.is_some());

        for width in [40, 80, 120, 160] {
            let mut list = UiState {
                workspace: Some((workspace.id, "workspace".into())),
                focus: Focus::Detail,
                detail: Some(crate::state::DetailState {
                    kind: crate::state::DetailKind::Changes,
                    title: "工作区变更".into(),
                }),
                detail_tab_selection: 1,
                ..UiState::default()
            };
            list.workspace_changes.page = Some(page.clone());
            list.workspace_changes.pages.back.push(None);
            let targets = assert_rendered_controls_have_identical_keyboard_and_mouse_actions(
                &mut list, width,
            );
            for expected in [
                HitTarget::WorkspaceChange(0),
                HitTarget::RefreshWorkspaceChanges,
                HitTarget::NewerWorkspaceChanges,
                HitTarget::OlderWorkspaceChanges,
            ] {
                assert!(
                    targets.contains(&expected),
                    "missing {expected:?} at {width}"
                );
            }

            let mut reader = list;
            reader.workspace_changes.file = Some(file.clone());
            reader.workspace_changes.file_back.push(None);
            let targets = assert_rendered_controls_have_identical_keyboard_and_mouse_actions(
                &mut reader,
                width,
            );
            for expected in [
                HitTarget::PreviousWorkspaceFile,
                HitTarget::MoreWorkspaceFile,
                HitTarget::CloseWorkspaceFile,
            ] {
                assert!(
                    targets.contains(&expected),
                    "missing {expected:?} at {width}"
                );
            }
        }

        app.shutdown().await.unwrap();
    }

    #[test]
    fn mouse_acceptance_uses_the_exact_fresh_result_or_does_nothing() {
        let mut state = UiState::default();
        assert!(matches!(
            hit_action(HitTarget::Accept, &state),
            Action::Noop
        ));

        let info = bone_app::SessionInfo {
            id: bone_app::SessionId::new(),
            workspace: bone_app::WorkspaceId::new(),
            title: "result".into(),
            archived: false,
        };
        let result = bone_app::ResultRef {
            session: info.id,
            job: bone_app::JobRef {
                runtime: bone_app::RuntimeId::new(),
                id: 1,
            },
            version: bone_app::SessionSeq(2),
        };
        state.sessions.push(info.clone());
        state.selected = Some(info.id);
        state
            .session_ui
            .insert(info.id, crate::state::SessionUi::new(info.clone(), 1));
        state.detail = Some(crate::state::DetailState {
            kind: crate::state::DetailKind::Acceptance,
            title: "结果与验收".into(),
        });
        state.results.insert(
            info.id,
            bone_app::ResultPage {
                items: vec![bone_app::ResultSummary {
                    result,
                    outcome: bone_app::OutcomeKind::Completed,
                    summary: "done".into(),
                    remaining: Vec::new(),
                }],
                older_cursor: None,
                snapshot_through: bone_app::SessionSeq(2),
                projection_pending: false,
            },
        );
        assert!(matches!(
            hit_action(HitTarget::Accept, &state),
            Action::BeginAcceptance {
                decision: bone_app::AcceptanceDecision::Accepted,
                result: target,
            } if target == result
        ));
        state.session_ui.get_mut(&info.id).unwrap().result_loading = true;
        assert!(matches!(
            hit_action(HitTarget::Accept, &state),
            Action::Noop
        ));
    }
}
