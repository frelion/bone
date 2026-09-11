use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph},
};

use crate::{
    state::{CommandSpec, UiState},
    ui::theme,
    view::{ACCENT, MUTED, PANEL, RAIL},
};

pub(super) fn render(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &UiState,
    matches: &[&CommandSpec],
    start: usize,
) {
    frame.render_widget(Clear, area);
    frame.render_widget(Block::default().style(theme::surface(RAIL)), area);
    for (index, command) in matches
        .iter()
        .enumerate()
        .skip(start)
        .take(usize::from(area.height))
    {
        let selected = index == state.slash_selection;
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    format!(" /{} {}", command.name, command.usage),
                    theme::label(if selected { PANEL } else { ACCENT }),
                ),
                Span::styled(
                    format!("  {}", command.summary),
                    Style::default().fg(if selected { PANEL } else { MUTED }),
                ),
            ]))
            .style(Style::default().bg(if selected { ACCENT } else { RAIL })),
            Rect::new(area.x, area.y + (index - start) as u16, area.width, 1),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selected_command_and_description_both_use_dark_ink() {
        let state = UiState::default();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 3)).unwrap();
        terminal
            .draw(|frame| {
                render(
                    frame,
                    Rect::new(0, 0, 80, 3),
                    &state,
                    &[&crate::state::COMMANDS[0]],
                    0,
                )
            })
            .unwrap();
        for x in 0..80 {
            let cell = &terminal.backend().buffer()[(x, 0)];
            assert_eq!(cell.bg, ACCENT);
            if cell.symbol() != " " {
                assert_eq!(cell.fg, PANEL);
            }
        }
    }
}
