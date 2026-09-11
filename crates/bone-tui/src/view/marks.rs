//! Shared vertical weight for selection and message/input markers.
use crate::ui::theme;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    text::Span,
};

// A quarter-cell block keeps the accent visible without becoming a full-cell bar.
// Do not inherit the text's bold attribute: marker weight is geometric.
pub(super) const VERTICAL: &str = "▎";

pub(super) fn span(tone: Color, background: Color) -> Span<'static> {
    Span::styled(VERTICAL, style(tone, background))
}

fn style(tone: Color, background: Color) -> Style {
    theme::regular(theme::body_on(tone, background))
}

pub(super) fn paint(frame: &mut Frame<'_>, area: Rect, tone: Color, background: Color) {
    for y in area.y..area.bottom() {
        frame.buffer_mut()[(area.x, y)]
            .set_symbol(VERTICAL)
            .set_style(style(tone, background));
    }
}
