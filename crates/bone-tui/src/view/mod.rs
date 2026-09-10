use crate::{layout::LayoutPlan, state::UiState};
use ratatui::{
    Frame,
    style::{Color, Style},
    widgets::{Block, Borders},
};

mod composer;
mod conversation;
mod message;
mod primitives;
mod session_rail;
mod slash_palette;

const INK: Color = Color::Rgb(217, 221, 225);
const MUTED: Color = Color::Rgb(116, 123, 130);
const PANEL: Color = Color::Rgb(13, 15, 17);
const RAIL: Color = Color::Rgb(16, 18, 20);
const ACCENT: Color = Color::Rgb(92, 168, 255);
const ATTENTION: Color = Color::Rgb(237, 180, 83);
const DANGER: Color = Color::Rgb(229, 107, 107);

/// Render the conversational shell and return its exact pointer hit map.
pub fn render(frame: &mut Frame<'_>, state: &UiState) -> LayoutPlan {
    let slash_matches = state.slash_matches();
    let selected_session = state
        .selected
        .and_then(|id| state.sessions.iter().position(|session| session.id == id));
    let mut plan = LayoutPlan::calculate(
        frame.area(),
        state.single_pane(),
        slash_matches.len(),
        state.sessions.len(),
        selected_session,
    );
    frame.render_widget(
        Block::default().style(Style::default().bg(PANEL)),
        plan.screen,
    );
    if let Some(area) = plan.session_rail {
        session_rail::render(frame, area, plan.session_start, state);
    }
    if let Some(area) = plan.extension_blank {
        frame.render_widget(
            Block::default()
                .borders(Borders::LEFT)
                .border_style(Style::default().fg(Color::Rgb(32, 36, 41)))
                .style(Style::default().bg(RAIL)),
            area,
        );
    }
    plan.transcript_metrics =
        conversation::render(frame, &plan, state, &slash_matches).map(std::sync::Arc::new);
    plan
}

/// Convert untrusted product text to inert display text.
pub fn sanitize_external(value: &str) -> String {
    value
        .chars()
        .filter(|character| {
            matches!(character, '\n' | '\t')
                || (!character.is_control()
                    && !matches!(
                        *character as u32,
                        0x061c | 0x200e | 0x200f | 0x202a..=0x202e | 0x2066..=0x2069
                    ))
        })
        .collect()
}

fn single_line_external(value: &str) -> String {
    sanitize_external(value).replace(['\n', '\t'], " ")
}
