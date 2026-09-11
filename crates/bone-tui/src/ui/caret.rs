//! Application-owned caret visibility.
//!
//! The terminal cursor remains positioned for IME and accessibility, while the
//! highlighted cell keeps the insertion point visible when the host cursor is
//! thin, blinking, or low contrast. This does not change host cursor settings.

use ratatui::Frame;

use super::theme;

pub(crate) fn place(frame: &mut Frame<'_>, position: (u16, u16), visible: bool) {
    if !visible {
        return;
    }
    // Set only the colors. A title keeps its label weight while the insertion
    // cell is lit, and no terminal cursor shape or color command is emitted.
    frame.buffer_mut()[position]
        .set_fg(theme::INPUT)
        .set_bg(theme::FOCUS_MARK);
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
                place(frame, (1, 0), true);
            })
            .unwrap();

        let cell = &terminal.backend().buffer()[(1, 0)];
        assert_eq!(cell.symbol(), "x");
        assert_eq!(cell.fg, theme::INPUT);
        assert_eq!(cell.bg, theme::FOCUS_MARK);
        assert_eq!(terminal.get_cursor_position().unwrap(), Position::new(1, 0));
    }

    #[test]
    fn hidden_phase_does_not_paint_or_show_the_native_cursor() {
        let mut terminal = Terminal::new(TestBackend::new(4, 1)).unwrap();
        terminal
            .draw(|frame| {
                frame.buffer_mut()[(1, 0)].set_symbol("x");
                place(frame, (1, 0), false);
            })
            .unwrap();
        let cell = &terminal.backend().buffer()[(1, 0)];
        assert_eq!(cell.symbol(), "x");
        assert_ne!(cell.bg, theme::FOCUS_MARK);
    }
}
