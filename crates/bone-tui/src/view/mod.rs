pub(crate) mod reader;
pub use crate::ui::frame::FrameSnapshot;

use crate::{
    layout::{HitRegion, HitTarget, LayoutPlan},
    state::UiState,
    ui::{interaction::HitMap, theme},
};
use ratatui::{Frame, style::Style, widgets::Block};

mod composer;
mod connection;
mod conversation;
mod marks;
mod message;
use crate::text as primitives;
mod panels;
mod session_rail;
mod slash_palette;

pub(super) use theme::{
    ACCENT, ATTENTION, CYAN, DANGER, DIVIDER, GREEN, INK, INPUT, MUTED, PANE_BOUNDARY, PANEL,
    PURPLE, RAIL, SELECTED, USER,
};

/// Render the conversational shell and return its exact pointer hit map.
pub fn render(frame: &mut Frame<'_>, state: &UiState) -> FrameSnapshot {
    let slash_matches = if state.focus == crate::state::Focus::Composer && state.panel.is_none() {
        state.slash_matches()
    } else {
        Vec::new()
    };
    let session_rows: Vec<u16> = (0..state.sessions.len())
        .map(|index| {
            let content = if session_rail::status(state, index).is_some() {
                2
            } else {
                1
            };
            content
                + 1
                + if crate::layout::comfortable(frame.area()) {
                    2
                } else {
                    0
                }
        })
        .collect();
    let input_width = state
        .pane_widths
        .content_width(frame.area())
        .saturating_sub(4);
    let draft_lines = crate::editor::editor_rows(state.draft(), input_width);
    let selected_session = state
        .session_candidate
        .or(state.selected)
        .and_then(|id| state.sessions.iter().position(|session| session.id == id));
    let mut plan = LayoutPlan::calculate_with_widths(
        frame.area(),
        state.single_pane(),
        slash_matches.len(),
        &session_rows,
        selected_session,
        draft_lines,
        state.pane_widths,
    );
    if let Some(start) = state.session_scroll {
        plan.scroll_sessions(&session_rows, start);
    }
    let slash_start = plan.slash_palette.map_or(0, |area| {
        state
            .slash_selection
            .saturating_sub(usize::from(area.height).saturating_sub(1))
    });
    let mut hits = base_hits(&plan, state, &slash_matches, slash_start);
    frame.render_widget(Block::default().style(theme::surface(PANEL)), plan.screen);
    session_rail::render(frame, &plan, state);
    if let Some(area) = plan.extension_blank {
        frame.render_widget(Block::default().style(Style::default().bg(RAIL)), area);
    }
    let transcript_metrics =
        conversation::render(frame, &plan, &mut hits, state, &slash_matches, slash_start)
            .map(std::sync::Arc::new);
    if plan.mode == crate::layout::LayoutMode::TooSmall {
        hits.clear();
        return FrameSnapshot::new(plan, hits, transcript_metrics, 0);
    }
    if state.panel.is_none()
        && slash_matches.is_empty()
        && let Some(area) = plan.composer
    {
        if !state.draft().trim().is_empty()
            && state
                .selected_ui()
                .and_then(|ui| ui.submitting.as_ref())
                .is_none_or(|pending| pending.failed)
        {
            hits.push(HitRegion {
                area: composer::action(area, state).0,
                target: HitTarget::Submit,
            });
        }
        if state.focus == crate::state::Focus::Composer
            && state
                .selected_ui()
                .is_some_and(|ui| ui.working() && ui.selected_answer.is_none())
        {
            let label = if state
                .selected_ui()
                .and_then(|ui| ui.snapshot.as_ref())
                .is_some_and(|snapshot| {
                    snapshot
                        .inputs
                        .iter()
                        .any(|input| matches!(input.state, bone_app::InputState::Queued { .. }))
                }) {
                "esc stop all"
            } else {
                "esc stop"
            };
            let stop_width = label.len() as u16;
            let stop = ratatui::layout::Rect::new(
                area.right().saturating_sub(stop_width + 2),
                area.y
                    .saturating_sub(if plan.screen.height < 18 { 1 } else { 2 }),
                stop_width.min(area.width),
                1,
            );
            frame.render_widget(
                ratatui::widgets::Paragraph::new(label).style(Style::default().fg(MUTED).bg(PANEL)),
                stop,
            );
            hits.push(HitRegion {
                area: stop,
                target: HitTarget::Stop,
            });
        }
    }
    if let Some(area) = plan.session_rail {
        hits.push(HitRegion {
            area: crate::layout::new_session_area(area),
            target: HitTarget::NewSession,
        });
    }
    if let Some(area) = plan.composer {
        let footer = composer::footer_areas(area, state);
        hits.push(HitRegion {
            area: footer.model,
            target: HitTarget::Models,
        });
        if let Some(commands) = footer.commands {
            hits.push(HitRegion {
                area: ratatui::layout::Rect::new(commands.x, commands.y, 10, 1),
                target: HitTarget::Commands,
            });
        }
    }
    let mut reader_max_scroll = 0;
    panels::render(frame, &plan, &mut hits, &mut reader_max_scroll, state);
    render_dividers(frame, &plan, &mut hits, state);
    FrameSnapshot::new(plan, hits, transcript_metrics, reader_max_scroll)
}

fn base_hits(
    plan: &LayoutPlan,
    state: &UiState,
    slash_matches: &[&crate::state::CommandSpec],
    slash_start: usize,
) -> HitMap {
    let mut hits = HitMap::default();
    if let Some(area) = plan.session_rail {
        hits.push(HitRegion {
            area,
            target: HitTarget::SessionRail,
        });
        for row in &plan.session_rows {
            if let Some(session) = state.sessions.get(row.index) {
                hits.push(HitRegion {
                    area: row.area,
                    target: HitTarget::Session(session.id),
                });
            }
        }
    }
    if let Some(area) = plan.transcript {
        hits.push(HitRegion {
            area,
            target: HitTarget::Conversation,
        });
    }
    if let Some(area) = plan.composer {
        hits.push(HitRegion {
            area,
            target: HitTarget::Composer,
        });
    }
    if let Some(area) = plan.slash_palette {
        for offset in 0..slash_matches.len().min(usize::from(area.height)) {
            if let Some(command) = slash_matches.get(slash_start + offset) {
                hits.push(HitRegion {
                    area: ratatui::layout::Rect::new(area.x, area.y + offset as u16, area.width, 1),
                    target: HitTarget::SlashCommand(command.kind),
                });
            }
        }
    }
    hits
}

fn render_dividers(frame: &mut Frame<'_>, plan: &LayoutPlan, hits: &mut HitMap, state: &UiState) {
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
        hits.push(HitRegion {
            area: ratatui::layout::Rect::new(x, plan.screen.y, 1, plan.screen.height),
            target: HitTarget::PaneDivider(divider),
        });
        let color = if state.dragging_divider == Some(divider) {
            ACCENT
        } else {
            PANE_BOUNDARY
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
                    assert_eq!(cell.bg == PANE_BOUNDARY, boundaries.contains(&x));
                }
                for x in &boundaries {
                    for y in 0..40 {
                        assert_eq!(buffer[(*x, y)].symbol(), " ");
                        assert_eq!(buffer[(*x, y)].bg, PANE_BOUNDARY);
                    }
                }
            }
        }
    }
}
