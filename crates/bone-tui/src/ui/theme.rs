//! Semantic visual tokens for the terminal UI.
//!
//! Components choose a role here instead of deciding font weight independently.
//! The terminal still owns the glyph shape; BONE owns the hierarchy, contrast,
//! spacing, and emphasis expressed through terminal cells.

use ratatui::style::{Color, Modifier, Style};

pub(crate) const INK: Color = Color::Rgb(238, 238, 238);
pub(crate) const MUTED: Color = Color::Rgb(174, 174, 174);
pub(crate) const PANEL: Color = Color::Rgb(18, 18, 18);
pub(crate) const RAIL: Color = Color::Rgb(9, 9, 9);
pub(crate) const INPUT: Color = Color::Rgb(32, 32, 32);
pub(crate) const USER: Color = Color::Rgb(26, 26, 26);
pub(crate) const SELECTED: Color = Color::Rgb(37, 37, 37);
pub(crate) const DIVIDER: Color = Color::Rgb(80, 80, 80);
pub(crate) const PANE_BOUNDARY: Color = Color::Rgb(40, 40, 40);
pub(crate) const ACCENT: Color = Color::Rgb(250, 178, 131);
pub(crate) const ATTENTION: Color = ACCENT;
pub(crate) const DANGER: Color = Color::Rgb(255, 102, 122);
pub(crate) const GREEN: Color = Color::Rgb(120, 224, 143);
pub(crate) const CYAN: Color = Color::Rgb(81, 214, 232);
pub(crate) const PURPLE: Color = Color::Rgb(202, 140, 255);

/// A surface never changes the weight inherited by content drawn over it.
pub(crate) fn surface(background: Color) -> Style {
    Style::default().bg(background)
}

/// Regular copy. This is the default for messages, hints, and editable text.
pub(crate) fn body(tone: Color) -> Style {
    Style::default().fg(tone)
}

pub(crate) fn body_on(tone: Color, background: Color) -> Style {
    body(tone).bg(background)
}

/// A short structural label: product name, panel title, or selected item.
pub(crate) fn label(tone: Color) -> Style {
    body(tone).add_modifier(Modifier::BOLD)
}

pub(crate) fn label_on(tone: Color, background: Color) -> Style {
    label(tone).bg(background)
}

/// Decorative marks must remain quiet even when painted over emphasized text.
pub(crate) fn regular(style: Style) -> Style {
    style.remove_modifier(Modifier::BOLD)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surfaces_do_not_make_descendants_bold() {
        let style = surface(PANEL);
        assert!(!style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn labels_are_the_only_bold_typography_role() {
        assert!(!body(INK).add_modifier.contains(Modifier::BOLD));
        assert!(label(INK).add_modifier.contains(Modifier::BOLD));
        assert!(!regular(label(INK)).add_modifier.contains(Modifier::BOLD));
    }
}
