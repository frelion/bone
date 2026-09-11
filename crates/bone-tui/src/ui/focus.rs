//! Focus ownership shared by every interactive region.
//!
//! Workspace regions choose their own visible expression: selection surface in
//! the rails and a caret in editors. Modal panels own the scope while open.

use crate::{
    state::{Focus, UiState},
    ui::theme,
};
use ratatui::{
    Frame,
    layout::Rect,
    style::Color,
    widgets::{Block, Widget},
};

pub(crate) const GUTTER_WIDTH: u16 = 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Region {
    Sessions,
    SessionTitle,
    Composer,
    RightRail,
    Panel,
}

pub(crate) fn is_active(state: &UiState, region: Region) -> bool {
    if state.panel.is_some() {
        return region == Region::Panel;
    }
    matches!(
        (state.focus, region),
        (Focus::Sessions, Region::Sessions)
            | (Focus::SessionTitle, Region::SessionTitle)
            | (Focus::Composer, Region::Composer)
            | (Focus::RightRail, Region::RightRail)
    )
}

/// Paint a fixed title band. Active read-only surfaces use the same neutral
/// selection lift as list rows; orange is reserved for identity and carets.
pub(crate) fn paint_header(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &UiState,
    region: Region,
    idle_background: Color,
) -> Color {
    paint_header_active(frame, area, is_active(state, region), idle_background)
}

/// Paint the shared header chrome for a child interaction that deliberately
/// keeps its parent workspace focus, such as Composer slash commands.
pub(crate) fn paint_header_active(
    frame: &mut Frame<'_>,
    area: Rect,
    active: bool,
    idle_background: Color,
) -> Color {
    let background = if active {
        theme::SELECTED
    } else {
        idle_background
    };
    Block::default()
        .style(theme::surface(background))
        .render(area, frame.buffer_mut());
    background
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend, style::Modifier};

    #[test]
    fn panel_exclusively_owns_focus_while_open() {
        for focus in [
            Focus::Sessions,
            Focus::SessionTitle,
            Focus::Composer,
            Focus::RightRail,
        ] {
            let mut state = UiState::default();
            state.focus = focus;
            assert!(is_active(
                &state,
                match focus {
                    Focus::Sessions => Region::Sessions,
                    Focus::SessionTitle => Region::SessionTitle,
                    Focus::Composer => Region::Composer,
                    Focus::RightRail => Region::RightRail,
                }
            ));
            state.panel = Some(crate::state::Panel::Help);
            assert!(is_active(&state, Region::Panel));
            assert!(!is_active(&state, Region::Sessions));
            assert!(!is_active(&state, Region::SessionTitle));
            assert!(!is_active(&state, Region::Composer));
            assert!(!is_active(&state, Region::RightRail));
        }
    }

    #[test]
    fn read_only_header_focus_uses_a_neutral_surface_without_orange() {
        let mut state = UiState::default();
        state.focus = Focus::SessionTitle;
        let mut terminal = Terminal::new(TestBackend::new(20, 2)).unwrap();
        terminal
            .draw(|frame| {
                paint_header(
                    frame,
                    Rect::new(2, 0, 16, 1),
                    &state,
                    Region::SessionTitle,
                    theme::PANEL,
                );
            })
            .unwrap();
        let cell = &terminal.backend().buffer()[(2, 0)];
        assert_eq!(cell.bg, theme::SELECTED);
        assert_ne!(cell.bg, theme::FOCUS_MARK);
        assert!(!cell.modifier.contains(Modifier::BOLD | Modifier::DIM));
    }
}
