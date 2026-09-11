pub(crate) mod reader;
pub use crate::ui::frame::FrameSnapshot;

use crate::{
    layout::{HitRegion, HitTarget, LayoutPlan},
    state::UiState,
    ui::{interaction::HitMap, theme},
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
pub fn render(frame: &mut Frame<'_>, state: &UiState) -> FrameSnapshot {
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
        .and_then(|id| state.sessions.iter().position(|session| session.id == id));
    let plan = LayoutPlan::calculate_with_widths(
        frame.area(),
        state.single_pane(),
        state.sessions.len(),
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
    let transcript_metrics = conversation::render(frame, &plan, &mut hits, state, &slash_matches)
        .map(std::sync::Arc::new);
    if plan.mode == crate::layout::LayoutMode::TooSmall {
        hits.clear();
        return FrameSnapshot::new(plan, hits, transcript_metrics, 0);
    }
    let mut reader_max_scroll = 0;
    panels::render(frame, &plan, &mut hits, &mut reader_max_scroll, state);
    // Overlays own the pointer scope. Pane boundaries remain visible behind a
    // panel, but they cannot be grabbed until the panel has closed.
    render_dividers(frame, &plan, &mut hits, state, state.panel.is_none());
    FrameSnapshot::new(plan, hits, transcript_metrics, reader_max_scroll)
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
mod region_tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend, style::Modifier};

    #[test]
    fn pane_boundaries_survive_overlays_and_disappear_with_hidden_panes() {
        for (width, boundaries) in [(160, vec![31, 120]), (120, vec![31]), (80, vec![])] {
            for panel in [None, Some(crate::state::Panel::Models)] {
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
        state.panel = Some(crate::state::Panel::Models);
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
