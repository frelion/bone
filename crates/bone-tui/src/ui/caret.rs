//! Application-owned caret visibility.
//!
//! The terminal cursor remains positioned for IME and accessibility, while the
//! highlighted cell keeps the insertion point visible when the host cursor is
//! thin, blinking, or low contrast. This does not change host cursor settings.

use ratatui::Frame;

use super::theme;

pub(crate) fn place(frame: &mut Frame<'_>, position: (u16, u16)) {
    frame.buffer_mut()[position]
        .set_style(theme::regular(theme::body_on(theme::INPUT, theme::ACCENT)));
    frame.set_cursor_position(position);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend, layout::Position};

    #[test]
    fn caret_keeps_the_cell_symbol_and_positions_the_native_cursor() {
        let mut terminal = Terminal::new(TestBackend::new(4, 1)).unwrap();
        terminal
            .draw(|frame| {
                frame.buffer_mut()[(1, 0)].set_symbol("x");
                place(frame, (1, 0));
            })
            .unwrap();

        let cell = &terminal.backend().buffer()[(1, 0)];
        assert_eq!(cell.symbol(), "x");
        assert_eq!(cell.fg, theme::INPUT);
        assert_eq!(cell.bg, theme::ACCENT);
        assert_eq!(terminal.get_cursor_position().unwrap(), Position::new(1, 0));
    }
}
