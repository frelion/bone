use crate::{
    layout::{HitRegion, HitTarget},
    state::{Action, Focus, UiState},
    ui::{
        focus,
        interaction::HitMap,
        theme::{self, INK, MUTED, RAIL, SELECTED},
    },
};
use ratatui::{
    Frame,
    layout::Rect,
    widgets::{Block, Paragraph},
};

pub(super) fn render(frame: &mut Frame<'_>, area: Rect, hits: &mut HitMap, state: &UiState) {
    hits.push(HitRegion {
        area,
        target: HitTarget::Action(Action::Focus(Focus::RightRail)),
    });
    let active = focus::workspace_focused(state, Focus::RightRail);
    let surface = if active { theme::FOCUS_SURFACE } else { RAIL };
    frame.render_widget(Block::default().style(theme::surface(surface)), area);

    let header = Rect::new(area.x + 3, area.y + 1, area.width.saturating_sub(6), 1);
    let header_background = if active { SELECTED } else { surface };
    frame.render_widget(
        Block::default().style(theme::surface(header_background)),
        header,
    );
    frame.render_widget(
        Paragraph::new("Details").style(theme::label_on(INK, header_background)),
        header,
    );
    frame.render_widget(
        Paragraph::new("Select a task or tool result").style(theme::body_on(MUTED, surface)),
        Rect::new(
            area.x + 3,
            area.y + 4,
            area.width.saturating_sub(6),
            2.min(area.height.saturating_sub(4)),
        ),
    );
    if active && area.height > 1 {
        frame.render_widget(
            Paragraph::new("ctrl+← return").style(theme::body_on(MUTED, surface)),
            Rect::new(
                area.x + 3,
                area.bottom() - 1,
                area.width.saturating_sub(6),
                1,
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn empty_right_rail_has_a_neutral_visible_focus_surface() {
        let mut state = UiState::default();
        state.focus = crate::state::Focus::RightRail;
        let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
        terminal
            .draw(|frame| {
                render(
                    frame,
                    Rect::new(0, 0, 40, 12),
                    &mut HitMap::default(),
                    &state,
                )
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(3, 1)].bg, SELECTED);
        assert_eq!(buffer[(3, 4)].bg, theme::FOCUS_SURFACE);
        assert_ne!(buffer[(3, 1)].bg, theme::FOCUS_MARK);
    }
}
