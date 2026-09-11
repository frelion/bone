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
/// A light, low-chroma selection surface that remains distinct from INPUT.
pub(crate) const SELECTED: Color = Color::Rgb(48, 48, 48);

/// Quiet, persistent boundaries between regions.
pub(crate) const STRUCTURE: Color = Color::Rgb(40, 40, 40);
/// A temporarily captured structural control, such as a dragged divider.
pub(crate) const STRUCTURE_ACTIVE: Color = Color::Rgb(112, 112, 112);
/// The only saturated workspace accent: current-session identity and carets.
pub(crate) const FOCUS_MARK: Color = Color::Rgb(250, 178, 131);
/// A one-row lift behind the active region title.
pub(crate) const FOCUS_SURFACE: Color = Color::Rgb(25, 25, 25);
pub(crate) const INFO: Color = Color::Rgb(81, 180, 198);
pub(crate) const SUCCESS: Color = Color::Rgb(120, 204, 140);
pub(crate) const WARNING: Color = Color::Rgb(224, 196, 92);
pub(crate) const DANGER: Color = Color::Rgb(255, 102, 122);

// Compatibility name for inline code rendering while it moves to semantic roles.
pub(crate) const CYAN: Color = INFO;
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
        for style in [surface(PANEL), body(INK), body_on(INK, PANEL)] {
            assert!(!style.add_modifier.contains(Modifier::BOLD));
        }
        assert!(label(INK).add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn semantic_states_do_not_reuse_the_focus_mark() {
        for tone in [STRUCTURE, STRUCTURE_ACTIVE, INFO, SUCCESS, WARNING, DANGER] {
            assert_ne!(tone, FOCUS_MARK);
        }
    }

    #[test]
    fn typography_roles_never_dim_content() {
        for style in [surface(PANEL), body(INK), body_on(INK, PANEL), label(INK)] {
            assert!(!style.add_modifier.contains(Modifier::DIM));
        }
    }
}
