pub(crate) mod reader;
pub use crate::ui::frame::FrameSnapshot;

use crate::{
    layout::{HitRegion, HitTarget, LayoutPlan},
    state::UiState,
    ui::{
        frame::{ComposerIdentity, ViewState},
        interaction::HitMap,
        theme,
    },
};
use ratatui::{Frame, widgets::Block};

mod composer;
mod connection;
mod conversation;
mod message;
mod panels;
mod right_rail;
mod session_rail;
mod slash_palette;

/// Render the conversational shell and return its exact pointer hit map.
#[cfg(test)]
pub(crate) fn render(frame: &mut Frame<'_>, state: &UiState) -> FrameSnapshot {
    render_with_view_state(frame, state, &mut ViewState::default())
}

pub(crate) fn render_with_view_state(
    frame: &mut Frame<'_>,
    state: &UiState,
    view_state: &mut ViewState,
) -> FrameSnapshot {
    let slash_visible = state.slash_palette_visible();
    let slash_matches = if slash_visible {
        state.slash_matches()
    } else {
        Vec::new()
    };
    let input_width = state
        .pane_widths
        .content_width(frame.area())
        .saturating_sub(4);
    let draft_lines = crate::editor::editor_rows(state.draft(), input_width);
    let selected_session = state
        .session_candidate
        .or(state.selected)
        .and_then(|id| state.session_rows.iter().position(|row| row.id() == id));
    let plan = LayoutPlan::calculate_with_widths(
        frame.area(),
        state.single_pane(),
        state.session_rows.len(),
        selected_session,
        state.session_scroll,
        draft_lines,
        state.pane_widths,
    );
    let mut hits = HitMap::default();
    frame.render_widget(
        Block::default().style(theme::surface(theme::PANEL)),
        plan.screen,
    );
    session_rail::render(frame, &plan, &mut hits, state);
    if let Some(area) = plan.extension_blank {
        right_rail::render(frame, area, &mut hits, state);
    }
    let composer_identity = composer_identity(state);
    view_state.retain_composers(|identity| composer_identity_exists(state, identity));
    let previous_composer_row = view_state.composer_origin(composer_identity);
    let conversation = conversation::render(
        frame,
        &plan,
        &mut hits,
        state,
        &slash_matches,
        previous_composer_row,
    );
    let transcript_metrics = conversation.metrics.map(std::sync::Arc::new);
    if let Some(origin) = conversation.composer_row_origin {
        view_state.remember_composer(composer_identity, origin);
    }
    if plan.mode == crate::layout::LayoutMode::TooSmall {
        hits.clear();
        return FrameSnapshot::new(
            plan,
            hits,
            transcript_metrics,
            0,
            conversation.composer_row_origin,
            conversation.title_byte_origin,
        );
    }
    let mut reader_max_scroll = 0;
    panels::render(frame, &plan, &mut hits, &mut reader_max_scroll, state);
    // Overlays own the pointer scope. Pane boundaries remain visible behind a
    // panel, but they cannot be grabbed until the panel has closed.
    render_dividers(frame, &plan, &mut hits, state, state.panel.is_none());
    FrameSnapshot::new(
        plan,
        hits,
        transcript_metrics,
        reader_max_scroll,
        conversation.composer_row_origin,
        conversation.title_byte_origin,
    )
}

fn composer_identity(state: &UiState) -> ComposerIdentity {
    let Some(ui) = state.selected_ui() else {
        return ComposerIdentity::Orphan;
    };
    if let Some(answer) = ui.active_answer() {
        ComposerIdentity::Answer {
            session: ui.id,
            question: answer.question,
        }
    } else {
        ComposerIdentity::Session { session: ui.id }
    }
}

fn composer_identity_exists(state: &UiState, identity: ComposerIdentity) -> bool {
    match identity {
        ComposerIdentity::Orphan => true,
        ComposerIdentity::Session { session } => state.session_ui.contains_key(&session),
        ComposerIdentity::Answer { session, question } => state
            .session_ui
            .get(&session)
            .is_some_and(|ui| ui.answer_drafts.contains_key(&question)),
    }
}

fn render_dividers(
    frame: &mut Frame<'_>,
    plan: &LayoutPlan,
    hits: &mut HitMap,
    state: &UiState,
    interactive: bool,
) {
    // Reserve existing edge whitespace; never take width from the reading or input area.
    let left = plan
        .session_rail
        .filter(|_| plan.conversation.is_some())
        .map(|area| area.right() - 1);
    let right = plan.extension_blank.map(|area| area.x);
    for (position, divider) in [
        (left, crate::layout::PaneDivider::Left),
        (right, crate::layout::PaneDivider::Right),
    ] {
        let Some(x) = position else {
            continue;
        };
        if interactive {
            hits.push(HitRegion {
                area: ratatui::layout::Rect::new(x, plan.screen.y, 1, plan.screen.height),
                target: HitTarget::PaneDivider(divider),
            });
        }
        let color = if interactive && state.dragging_divider == Some(divider) {
            theme::STRUCTURE_ACTIVE
        } else {
            theme::STRUCTURE
        };
        for y in plan.screen.y..plan.screen.bottom() {
            frame.buffer_mut()[(x, y)]
                .set_symbol(" ")
                .set_fg(color)
                .set_bg(color);
        }
    }
}

pub use crate::text::sanitize_external;

fn single_line_external(value: &str) -> String {
    sanitize_external(value).replace(['\n', '\t'], " ")
}

#[cfg(test)]
mod editor_view_state_tests {
    use super::*;
    use crate::{
        editor::{CursorMove, EditCommand},
        state::{SessionNavRow, SessionUi},
    };
    use ratatui::{Terminal, backend::TestBackend};

    fn draw(state: &UiState, view_state: &mut ViewState, width: u16, height: u16) -> FrameSnapshot {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let mut snapshot = None;
        terminal
            .draw(|frame| {
                snapshot = Some(render_with_view_state(frame, state, view_state));
            })
            .unwrap();
        snapshot.unwrap()
    }

    fn long_draft(label: &str) -> crate::editor::EditorBuffer {
        (0..20)
            .map(|row| format!("{label} row {row}"))
            .collect::<Vec<_>>()
            .join("\n")
            .into()
    }

    fn fixture() -> (UiState, bone_app::SessionId, bone_app::SessionId) {
        let workspace = bone_app::WorkspaceId::new();
        let first = bone_app::SessionInfo {
            id: bone_app::SessionId::new(),
            workspace,
            title: "first".into(),
            archived: false,
        };
        let second = bone_app::SessionInfo {
            id: bone_app::SessionId::new(),
            workspace,
            title: "second".into(),
            archived: false,
        };
        let mut first_ui = SessionUi::new(first.id, 1);
        first_ui.draft = long_draft("first");
        let mut second_ui = SessionUi::new(second.id, 1);
        second_ui.draft = long_draft("second");
        let mut state = UiState::default();
        state.session_rows = vec![
            SessionNavRow::provisional(first.clone()),
            SessionNavRow::provisional(second.clone()),
        ];
        state.selected = Some(first.id);
        state.session_candidate = Some(first.id);
        state.session_ui.insert(first.id, first_ui);
        state.session_ui.insert(second.id, second_ui);
        (state, first.id, second.id)
    }

    fn move_up_within_viewport(state: &mut UiState, snapshot: &FrameSnapshot) {
        let width = crate::layout::composer_text_area(snapshot.layout.composer.unwrap()).width;
        state.editor_mut().apply(EditCommand::Move {
            cursor: CursorMove::Up { width },
            select: false,
        });
    }

    #[test]
    fn each_session_and_answer_keeps_its_own_bounded_viewport_origin() {
        let (mut state, first, second) = fixture();
        let mut view_state = ViewState::default();

        let first_frame = draw(&state, &mut view_state, 80, 12);
        let first_origin = first_frame.composer_row_origin().unwrap();
        assert!(first_origin > 0);
        move_up_within_viewport(&mut state, &first_frame);
        assert_eq!(
            draw(&state, &mut view_state, 80, 12).composer_row_origin(),
            Some(first_origin)
        );

        state.selected = Some(second);
        state.session_candidate = Some(second);
        let second_frame = draw(&state, &mut view_state, 80, 12);
        move_up_within_viewport(&mut state, &second_frame);
        state.selected = Some(first);
        state.session_candidate = Some(first);
        assert_eq!(
            draw(&state, &mut view_state, 80, 12).composer_row_origin(),
            Some(first_origin)
        );

        let question = bone_app::QuestionId {
            runtime: bone_app::RuntimeId::new(),
            record: 1,
            reply_to: bone_app::InputId(1),
        };
        let ui = state.session_ui.get_mut(&first).unwrap();
        let mut answer = crate::state::answer::AnswerDraft::new(question);
        answer.editor = long_draft("answer");
        ui.answer_drafts.insert(question, answer);
        ui.selected_answer = Some(question);
        let answer_frame = draw(&state, &mut view_state, 80, 12);
        let answer_origin = answer_frame.composer_row_origin().unwrap();
        move_up_within_viewport(&mut state, &answer_frame);
        assert_eq!(
            draw(&state, &mut view_state, 80, 12).composer_row_origin(),
            Some(answer_origin)
        );

        state.session_ui.get_mut(&first).unwrap().selected_answer = None;
        assert_eq!(
            draw(&state, &mut view_state, 80, 12).composer_row_origin(),
            Some(first_origin)
        );
        state.session_ui.get_mut(&first).unwrap().selected_answer = Some(question);
        assert_eq!(
            draw(&state, &mut view_state, 80, 12).composer_row_origin(),
            Some(answer_origin)
        );
    }

    #[test]
    fn too_small_frame_does_not_erase_the_active_buffer_origin() {
        let (mut state, first, _) = fixture();
        let mut view_state = ViewState::default();
        let frame = draw(&state, &mut view_state, 80, 12);
        let origin = frame.composer_row_origin().unwrap();
        move_up_within_viewport(&mut state, &frame);
        assert_eq!(
            draw(&state, &mut view_state, 80, 12).composer_row_origin(),
            Some(origin)
        );

        let small = draw(&state, &mut view_state, 39, 11);
        assert_eq!(small.layout.mode, crate::layout::LayoutMode::TooSmall);
        state.session_ui.get_mut(&first).unwrap().generation += 1;
        assert_eq!(
            draw(&state, &mut view_state, 80, 12).composer_row_origin(),
            Some(origin)
        );
    }
}

#[cfg(test)]
mod region_tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend, style::Modifier};

    #[test]
    fn pane_boundaries_survive_overlays_and_disappear_with_hidden_panes() {
        for (width, boundaries) in [(160, vec![31, 120]), (120, vec![31]), (80, vec![])] {
            for panel in [
                None,
                Some(crate::state::Panel::Models(crate::state::ModelPanel::new(
                    None,
                ))),
            ] {
                let mut terminal = Terminal::new(TestBackend::new(width, 40)).unwrap();
                let mut state = UiState::default();
                state.panel = panel;
                terminal
                    .draw(|frame| {
                        render(frame, &state);
                    })
                    .unwrap();
                let buffer = terminal.backend().buffer();
                let text_cells: Vec<_> = buffer
                    .content
                    .iter()
                    .filter(|cell| cell.symbol().chars().any(char::is_alphanumeric))
                    .collect();
                assert!(
                    text_cells
                        .iter()
                        .any(|cell| cell.modifier.contains(Modifier::BOLD))
                );
                assert!(
                    text_cells
                        .iter()
                        .any(|cell| !cell.modifier.contains(Modifier::BOLD))
                );
                for x in 0..width {
                    let cell = &buffer[(x, 0)];
                    assert_eq!(cell.bg == theme::STRUCTURE, boundaries.contains(&x));
                }
                for x in &boundaries {
                    for y in 0..40 {
                        assert_eq!(buffer[(*x, y)].symbol(), " ");
                        assert_eq!(buffer[(*x, y)].bg, theme::STRUCTURE);
                    }
                }
            }
        }
    }

    #[test]
    fn overlays_keep_boundaries_visual_but_remove_their_pointer_targets() {
        let mut terminal = Terminal::new(TestBackend::new(160, 40)).unwrap();
        let mut state = UiState::default();
        state.panel = Some(crate::state::Panel::Models(crate::state::ModelPanel::new(
            None,
        )));
        state.dragging_divider = Some(crate::layout::PaneDivider::Left);
        let mut snapshot = None;
        terminal
            .draw(|frame| snapshot = Some(render(frame, &state)))
            .unwrap();

        let snapshot = snapshot.unwrap();
        assert!(snapshot.hit_regions().iter().all(|hit| {
            !matches!(hit.target, HitTarget::PaneDivider(_))
                && !matches!(
                    hit.target,
                    HitTarget::SessionRail
                        | HitTarget::Session(_)
                        | HitTarget::Conversation
                        | HitTarget::Composer
                )
        }));
        for y in 0..40 {
            assert_eq!(terminal.backend().buffer()[(31, y)].bg, theme::STRUCTURE);
        }
    }

    #[test]
    fn divider_drag_uses_neutral_structure_color_instead_of_focus_orange() {
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        let mut state = UiState::default();
        state.dragging_divider = Some(crate::layout::PaneDivider::Left);
        terminal.draw(|frame| _ = render(frame, &state)).unwrap();

        for y in 0..40 {
            let cell = &terminal.backend().buffer()[(31, y)];
            assert_eq!(cell.bg, theme::STRUCTURE_ACTIVE);
            assert_ne!(cell.bg, theme::FOCUS_MARK);
        }
    }
}
