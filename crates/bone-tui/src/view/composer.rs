use crate::{
    input::{BindingHint, status_baseline_bindings},
    state::{Focus, UiState},
    ui::{caret, theme},
    view::{ACCENT, INK, INPUT, MUTED, single_line_external},
};
use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Block, Paragraph},
};

pub(super) fn render(frame: &mut Frame<'_>, area: Rect, state: &UiState) {
    let draft = state.draft();
    let cursor = state.draft_cursor();
    // Slash suggestions are attached to the editor and must not steal its caret.
    let focused = state.focus == Focus::Composer && state.panel.is_none();
    let surface = Rect::new(area.x, area.y, area.width, area.height.saturating_sub(2));
    frame.render_widget(Block::default().style(Style::default().bg(INPUT)), surface);
    super::marks::paint(
        frame,
        surface,
        if focused { ACCENT } else { super::DIVIDER },
        INPUT,
    );
    let input = crate::layout::composer_text_area(area);
    let (value, cursor_x, cursor_y) = crate::editor::stable_editor_viewport(
        draft,
        cursor,
        input.width,
        input.height,
        state.editor().viewport(),
    );
    frame.render_widget(
        Paragraph::new(if draft.is_empty() {
            "Write a request…"
        } else {
            &value
        })
        .style(
            Style::default()
                .fg(if draft.is_empty() { MUTED } else { INK })
                .bg(INPUT),
        ),
        input,
    );
    if focused && let Some(selection) = state.editor().selection(cursor) {
        for (x, y, width) in crate::editor::selection_cells(
            draft,
            input.width,
            input.height,
            state.editor().viewport_origin(),
            selection,
        ) {
            for dx in 0..width {
                frame.buffer_mut()[(input.x + x + dx, input.y + y)]
                    .set_bg(super::SELECTED)
                    .set_fg(INK);
            }
        }
    }
    let model = single_line_external(&state.model_footer());
    let geometry = footer_areas(area, state);
    let (action_area, action) = action(area, state);
    frame.render_widget(
        Paragraph::new(model).style(theme::body_on(MUTED, super::PANEL)),
        geometry.model,
    );
    if state.panel.is_none()
        && let Some(bindings) = geometry.bindings
    {
        let hints = status_baseline_bindings(state.terminal_capabilities.shift_enter_supported());
        frame.render_widget(
            Paragraph::new(binding_line(&hints, geometry.binding_count))
                .style(theme::surface(super::PANEL)),
            bindings,
        );
    }
    if state.panel.is_none()
        && let Some(commands) = geometry.commands
    {
        frame.render_widget(
            Paragraph::new("/ commands").style(Style::default().fg(MUTED)),
            commands,
        );
    }
    if state.panel.is_none() {
        frame.render_widget(
            Paragraph::new(action).style(if focused {
                theme::label(ACCENT)
            } else {
                theme::body(MUTED)
            }),
            action_area,
        );
    }
    if focused && input.width > 0 && input.height > 0 {
        caret::place(frame, (input.x + cursor_x, input.y + cursor_y));
    }
}

/// Shared by paint and pointer registration, including intermediate pane widths.
pub(super) struct FooterAreas {
    pub model: Rect,
    pub commands: Option<Rect>,
    pub bindings: Option<Rect>,
    pub binding_count: usize,
}

fn binding_width(hints: &[BindingHint]) -> u16 {
    hints
        .iter()
        .map(|hint| hint.chord.len() + 1 + hint.label.len())
        .sum::<usize>()
        .saturating_add(hints.len().saturating_sub(1) * 2) as u16
}

fn binding_line(hints: &[BindingHint], count: usize) -> Line<'static> {
    let mut spans = Vec::new();
    for (index, hint) in hints.iter().take(count).enumerate() {
        if index > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(hint.chord, theme::label(INK)));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(hint.label, theme::body(MUTED)));
    }
    Line::from(spans)
}

pub(super) fn footer_areas(area: Rect, state: &UiState) -> FooterAreas {
    let footer = Rect::new(
        area.x + 2,
        area.bottom().saturating_sub(1),
        area.width.saturating_sub(4),
        1,
    );
    let hints = status_baseline_bindings(state.terminal_capabilities.shift_enter_supported());
    let action_area = action(area, state).0;
    let mut right = action_area.x.saturating_sub(2).max(footer.x);
    let available = right.saturating_sub(footer.x);
    let minimum_model = available.min(16);

    let mut binding_count = 0;
    let mut bindings = None;
    for count in (1..=hints.len()).rev() {
        let width = binding_width(&hints[..count]);
        if width.saturating_add(2).saturating_add(minimum_model) <= available {
            let x = right.saturating_sub(width);
            bindings = Some(Rect::new(x, footer.y, width, 1));
            binding_count = count;
            right = x.saturating_sub(2).max(footer.x);
            break;
        }
    }

    let commands =
        (right.saturating_sub(footer.x) >= minimum_model.saturating_add(12)).then(|| {
            right = right.saturating_sub(10);
            let area = Rect::new(right, footer.y, 10, 1);
            right = right.saturating_sub(2).max(footer.x);
            area
        });

    FooterAreas {
        model: Rect::new(footer.x, footer.y, right.saturating_sub(footer.x), 1),
        commands,
        bindings,
        binding_count,
    }
}

pub(super) fn action(area: Rect, state: &UiState) -> (Rect, &'static str) {
    let session = state.selected_ui();
    let pending = session
        .and_then(|ui| ui.submitting.as_ref())
        .is_some_and(|pending| !pending.failed);
    let answering = session.is_some_and(|ui| ui.selected_answer.is_some());
    let label = if pending {
        "Saving…"
    } else if answering {
        if session.is_some_and(|ui| {
            ui.active_answer().is_some_and(|answer| {
                ui.snapshot.as_ref().is_none_or(|snapshot| {
                    crate::state::answer::active_question(snapshot, answer.question).is_none()
                })
            })
        }) {
            "answer ended"
        } else {
            "enter answer"
        }
    } else if state.model_label.is_none() && state.running_model().is_none() {
        "enter save"
    } else if session.is_some_and(|ui| ui.working()) {
        "enter append"
    } else {
        "enter send"
    };
    let width = unicode_width::UnicodeWidthStr::width(label) as u16;
    (
        Rect::new(
            area.right().saturating_sub(2 + width),
            area.bottom() - 1,
            width,
            1,
        ),
        label,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::HitTarget;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn input_padding_model_and_shortcuts_have_separate_rows() {
        for (width, height) in [(40, 12), (80, 24), (120, 30), (160, 40)] {
            for lines in [1, 2, 20] {
                let mut state = UiState::default();
                state.orphan_draft = vec!["draft"; lines].join("\n");
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                terminal
                    .draw(|frame| {
                        let plan = crate::view::render(frame, &state);
                        let area = plan.composer.unwrap();
                        let input = crate::layout::composer_text_area(area);
                        let footer = footer_areas(area, &state);
                        assert_eq!(input.y, area.y + 1);
                        assert_eq!(footer.model.y, input.bottom() + 2);
                        assert_eq!(action(area, &state).0.y, footer.model.y);
                        assert!(plan.transcript.unwrap().height >= 3);
                        assert!(area.height <= 9);
                        for y in [area.y, input.bottom()] {
                            for x in area.x + 1..area.right() {
                                assert_eq!(frame.buffer_mut()[(x, y)].symbol(), " ");
                                assert_eq!(frame.buffer_mut()[(x, y)].bg, INPUT);
                            }
                        }
                        assert_eq!(
                            frame.buffer_mut()[(footer.model.x, footer.model.y)].bg,
                            super::super::PANEL
                        );
                    })
                    .unwrap();
            }
        }
    }

    #[test]
    fn minimum_screen_keeps_full_width_draft_visible_at_end_cursor() {
        let mut state = UiState::default();
        state.orphan_draft = "draft survives model setup".into();
        state.orphan_cursor = state.orphan_draft.len();
        let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
        terminal
            .draw(|frame| {
                crate::view::render(frame, &state);
            })
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("draft survives model setup"));
    }

    #[test]
    fn slash_palette_keeps_the_composer_caret_visible() {
        let mut state = UiState::default();
        state.orphan_draft = "/".into();
        state.orphan_cursor = state.orphan_draft.len();
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        let mut plan = None;

        terminal
            .draw(|frame| plan = Some(crate::view::render(frame, &state)))
            .unwrap();

        let input = crate::layout::composer_text_area(plan.unwrap().composer.unwrap());
        let (_, cursor_x, cursor_y) = crate::editor::stable_editor_viewport(
            state.draft(),
            state.draft_cursor(),
            input.width,
            input.height,
            state.editor().viewport(),
        );
        let position = ratatui::layout::Position::new(input.x + cursor_x, input.y + cursor_y);
        assert_eq!(terminal.get_cursor_position().unwrap(), position);
        let cell = &terminal.backend().buffer()[position];
        assert_eq!(cell.bg, ACCENT);
        assert_eq!(cell.fg, INPUT);
    }

    #[test]
    fn inactive_composer_preserves_text_without_action_hints() {
        let mut state = UiState::default();
        state.orphan_draft = "preserved draft".into();
        state.panel = Some(crate::state::Panel::Models);
        let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
        terminal
            .draw(|frame| render(frame, Rect::new(0, 0, 72, 5), &state))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let text: String = (0..5)
            .flat_map(|y| (0..72).map(move |x| (x, y)))
            .map(|position| buffer[position].symbol())
            .collect();
        assert!(text.contains("preserved draft"));
        assert!(text.contains("Select model"));
        assert!(!text.contains("enter save"));
        assert!(!text.contains("/ commands"));
        assert!(!text.contains("ctrl+c clear"));
        assert!(!text.contains("ctrl+d exit"));
        assert!(!text.contains('›'));
    }

    #[test]
    fn visible_footer_actions_share_exact_pointer_geometry_at_breakpoints() {
        for width in [40, 60, 80, 100, 120, 140, 144, 160, 220] {
            let mut state = UiState::default();
            state.orphan_draft = "hello".into();
            let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
            let mut plan = None;
            terminal
                .draw(|frame| {
                    plan = Some(crate::view::render(frame, &state));
                })
                .unwrap();
            let plan = plan.unwrap();
            let area = plan.composer.unwrap();
            let geometry = footer_areas(area, &state);
            let buffer = terminal.backend().buffer();
            for offset in 0.."Select model".len().min(usize::from(geometry.model.width)) {
                assert_eq!(
                    plan.hit(geometry.model.x + offset as u16, geometry.model.y),
                    Some(HitTarget::Models),
                    "width {width}"
                );
            }
            if let Some(commands) = geometry.commands {
                let painted: String = (commands.x..commands.x + 10)
                    .map(|x| buffer[(x, commands.y)].symbol())
                    .collect();
                assert_eq!(painted, "/ commands");
                for x in commands.x..commands.x + 10 {
                    assert_eq!(
                        plan.hit(x, commands.y),
                        Some(HitTarget::Commands),
                        "width {width}"
                    );
                }
                assert_ne!(
                    plan.hit(commands.x + 11, commands.y),
                    Some(HitTarget::Commands)
                );
            } else {
                assert!(
                    !plan
                        .hit_regions()
                        .iter()
                        .any(|region| region.target == HitTarget::Commands),
                    "hidden commands at width {width}"
                );
            }
            let submit = action(area, &state).0;
            assert_eq!(plan.hit(submit.x, submit.y), Some(HitTarget::Submit));
        }
    }

    #[test]
    fn status_baseline_is_rendered_from_the_exact_keymap_contract() {
        let state = UiState::default();
        let area = Rect::new(0, 0, 100, 6);
        let geometry = footer_areas(area, &state);
        assert_eq!(geometry.binding_count, 3);

        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal.draw(|frame| render(frame, area, &state)).unwrap();
        let buffer = terminal.backend().buffer();
        let bindings = geometry.bindings.unwrap();
        let text: String = (bindings.x..bindings.right())
            .map(|x| buffer[(x, bindings.y)].symbol())
            .collect();

        for hint in status_baseline_bindings(true) {
            assert!(
                text.contains(&format!("{} {}", hint.chord, hint.label)),
                "missing visible binding {hint:?}"
            );
            let chord_start = text.find(hint.chord).unwrap() as u16;
            for x in chord_start..chord_start + hint.chord.len() as u16 {
                assert!(
                    buffer[(bindings.x + x, bindings.y)]
                        .modifier
                        .contains(ratatui::style::Modifier::BOLD)
                );
            }
            let label_start = text.find(hint.label).unwrap() as u16;
            for x in label_start..label_start + hint.label.len() as u16 {
                assert!(
                    !buffer[(bindings.x + x, bindings.y)]
                        .modifier
                        .contains(ratatui::style::Modifier::BOLD)
                );
            }
        }
    }

    #[test]
    fn compatibility_profile_marks_shift_enter_as_unavailable() {
        let mut state = UiState::default();
        state.terminal_capabilities =
            crate::terminal::TerminalCapabilities::compatibility("test terminal");
        let area = Rect::new(0, 0, 100, 6);
        let geometry = footer_areas(area, &state);
        assert_eq!(geometry.binding_count, 3);

        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal.draw(|frame| render(frame, area, &state)).unwrap();
        let buffer = terminal.backend().buffer();
        let bindings = geometry.bindings.unwrap();
        let text: String = (bindings.x..bindings.right())
            .map(|x| buffer[(x, bindings.y)].symbol())
            .collect();

        assert!(text.contains("shift+enter unavailable"));
        assert!(!text.contains("shift+enter newline"));
    }
}
