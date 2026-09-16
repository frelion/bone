use crate::ui::theme::{self, INK, MUTED, RAIL};
use ratatui::{
    Frame,
    layout::Rect,
    widgets::{Block, Paragraph},
};

pub(super) fn render(frame: &mut Frame<'_>, area: Rect) {
    frame.render_widget(Block::default().style(theme::surface(RAIL)), area);
    frame.render_widget(
        Paragraph::new("Details").style(theme::label_on(INK, RAIL)),
        Rect::new(area.x + 3, area.y + 1, area.width.saturating_sub(6), 1),
    );
    frame.render_widget(
        Paragraph::new("Select a task or tool result").style(theme::body_on(MUTED, RAIL)),
        Rect::new(
            area.x + 3,
            area.y + 4,
            area.width.saturating_sub(6),
            2.min(area.height.saturating_sub(4)),
        ),
    );
}
