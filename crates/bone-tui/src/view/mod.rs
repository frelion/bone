pub(crate) mod reader;
use crate::{layout::LayoutPlan, state::UiState};
use ratatui::{
    Frame,
    style::{Color, Style},
    widgets::Block,
};

mod composer;
mod connection;
mod conversation;
mod message;
use crate::text as primitives;
mod panels;
mod session_rail;
mod slash_palette;

const INK: Color = Color::Rgb(245, 245, 245);
const MUTED: Color = Color::Rgb(171, 171, 171);
const PANEL: Color = Color::Rgb(9, 9, 9);
const RAIL: Color = PANEL;
const INPUT: Color = Color::Rgb(34, 34, 34);
const USER: Color = Color::Rgb(25, 25, 25);
const SELECTED: Color = Color::Rgb(37, 37, 37);
const DIVIDER: Color = Color::Rgb(80, 80, 80);
const ACCENT: Color = Color::Rgb(255, 157, 36);
const ATTENTION: Color = ACCENT;
const DANGER: Color = Color::Rgb(255, 102, 122);
const GREEN: Color = Color::Rgb(120, 224, 143);
const CYAN: Color = Color::Rgb(81, 214, 232);
const PURPLE: Color = Color::Rgb(202, 140, 255);

/// Render the conversational shell and return its exact pointer hit map.
pub fn render(frame: &mut Frame<'_>, state: &UiState) -> LayoutPlan {
    let slash_matches = if state.focus == crate::state::Focus::Composer && state.panel.is_none() {
        state.slash_matches()
    } else {
        Vec::new()
    };
    let session_rows: Vec<u16> = (0..state.sessions.len())
        .map(|index| {
            if session_rail::status(state, index).is_some() {
                3
            } else {
                2
            }
        })
        .collect();
    let input_width = LayoutPlan::content_width(frame.area()).saturating_sub(6);
    let draft_lines = primitives::editor_rows(state.draft(), input_width);
    let selected_session = state
        .session_candidate
        .or(state.selected)
        .and_then(|id| state.sessions.iter().position(|session| session.id == id));
    let mut plan = LayoutPlan::calculate(
        frame.area(),
        state.single_pane(),
        slash_matches.len(),
        &session_rows,
        selected_session,
        draft_lines,
    );
    if let Some(start) = state.session_scroll {
        plan.scroll_sessions(&session_rows, start);
    }
    for region in &mut plan.hit_regions {
        if matches!(region.target, crate::layout::HitTarget::Session(_)) {
            // The final layout row is a gap, not a clickable part of the session.
            region.area.height -= 1;
        }
    }
    if let Some(area) = plan.slash_palette {
        plan.slash_start = state
            .slash_selection
            .saturating_sub(usize::from(area.height).saturating_sub(1));
        for region in &mut plan.hit_regions {
            if let crate::layout::HitTarget::SlashCommand(index) = &mut region.target {
                *index += plan.slash_start;
            }
        }
    }
    frame.render_widget(
        Block::default().style(Style::default().bg(PANEL)),
        plan.screen,
    );
    session_rail::render(frame, &plan, state);
    if let Some(area) = plan.extension_blank {
        frame.render_widget(Block::default().style(Style::default().bg(RAIL)), area);
    }
    plan.transcript_metrics =
        conversation::render(frame, &mut plan, state, &slash_matches).map(std::sync::Arc::new);
    if plan.mode == crate::layout::LayoutMode::TooSmall {
        plan.hit_regions.clear();
        return plan;
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
            plan.hit_regions.push(crate::layout::HitRegion {
                area: composer::action(area, state).0,
                target: crate::layout::HitTarget::Submit,
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
            plan.hit_regions.push(crate::layout::HitRegion {
                area: stop,
                target: crate::layout::HitTarget::Stop,
            });
        }
    }
    if let Some(area) = plan.session_rail {
        plan.hit_regions.push(crate::layout::HitRegion {
            area: ratatui::layout::Rect::new(
                area.x + 3,
                area.y + 2,
                area.width.saturating_sub(6),
                1,
            ),
            target: crate::layout::HitTarget::NewSession,
        });
    }
    if let Some(area) = plan.composer {
        let footer = composer::footer_areas(area, state);
        plan.hit_regions.push(crate::layout::HitRegion {
            area: footer.model,
            target: crate::layout::HitTarget::Models,
        });
        if let Some(commands) = footer.commands {
            plan.hit_regions.push(crate::layout::HitRegion {
                area: ratatui::layout::Rect::new(commands.x, commands.y, 10, 1),
                target: crate::layout::HitTarget::Commands,
            });
        }
    }
    panels::render(frame, &mut plan, state);
    render_dividers(frame, &plan);
    plan
}

fn render_dividers(frame: &mut Frame<'_>, plan: &LayoutPlan) {
    // Reserve existing edge whitespace; never take width from the reading or input area.
    let left = plan
        .session_rail
        .filter(|_| plan.conversation.is_some())
        .map(|area| area.right() - 1);
    let right = plan.extension_blank.map(|area| area.x);
    for x in [left, right].into_iter().flatten() {
        for y in plan.screen.y..plan.screen.bottom() {
            frame.buffer_mut()[(x, y)]
                .set_symbol("│")
                .set_fg(DIVIDER)
                .set_bg(PANEL);
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
    use ratatui::{Terminal, backend::TestBackend};

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
                for x in 0..width {
                    let cell = &buffer[(x, 0)];
                    assert_eq!(cell.symbol() == "│", boundaries.contains(&x));
                }
                for x in &boundaries {
                    for y in 0..40 {
                        assert_eq!(buffer[(*x, y)].symbol(), "│");
                        assert_eq!(buffer[(*x, y)].fg, DIVIDER);
                    }
                }
            }
        }
    }
}
