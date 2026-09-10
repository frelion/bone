use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph},
};

use crate::{
    state::{CommandSpec, UiState},
    view::{ACCENT, INK, MUTED, RAIL},
};

pub(super) fn render(frame: &mut Frame<'_>, area: Rect, state: &UiState, matches: &[&CommandSpec]) {
    frame.render_widget(Clear, area);
    frame.render_widget(Block::default().style(Style::default().bg(RAIL)), area);
    for (index, command) in matches.iter().take(usize::from(area.height)).enumerate() {
        let selected = index == state.slash_selection;
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    format!(" /{} {}", command.name, command.usage),
                    Style::default()
                        .fg(if selected { INK } else { ACCENT })
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("  {}", command.summary), Style::default().fg(MUTED)),
            ]))
            .style(Style::default().bg(if selected {
                Color::Rgb(42, 49, 58)
            } else {
                RAIL
            })),
            Rect::new(area.x, area.y + index as u16, area.width, 1),
        );
    }
}
