use ratatui::{Frame, layout::Rect, widgets::Paragraph};

use crate::{
    layout::{HitRegion, HitTarget, attached_floating_panel_area, floating_menu_stride},
    state::{Action, CommandSpec, UiState},
    ui::{interaction::HitMap, theme},
};

pub(super) fn render(
    frame: &mut Frame<'_>,
    screen: Rect,
    composer: Rect,
    hits: &mut HitMap,
    state: &UiState,
    matches: &[&CommandSpec],
) {
    let stride = floating_menu_stride(screen);
    let area = attached_floating_panel_area(
        screen,
        composer,
        (matches.len().max(1) as u16)
            .saturating_mul(stride)
            .saturating_add(3),
    );
    let shell = super::panels::render_shell(frame, screen, area, "Commands");

    // The complete surface captures pointer input so its title, padding and
    // blank rows cannot fall through to the conversation underneath it.
    hits.push(HitRegion {
        area,
        target: HitTarget::Capture,
    });
    hits.push(HitRegion {
        area: shell.back,
        target: HitTarget::Action(Action::Escape),
    });

    let capacity = usize::from(shell.inner.height / shell.stride).max(1);
    let start = state
        .slash_selection
        .saturating_sub(capacity.saturating_sub(1));
    if matches.is_empty() {
        frame.render_widget(
            Paragraph::new("No matching commands")
                .style(theme::body_on(theme::MUTED, theme::INPUT)),
            shell.inner,
        );
        return;
    }

    for (index, command) in matches.iter().enumerate().skip(start).take(capacity) {
        let row = Rect::new(
            shell.inner.x,
            shell.inner.y + (index - start) as u16 * shell.stride,
            shell.inner.width,
            shell.stride,
        );
        let label = if shell.inner.width >= 52 {
            format!("/{:<12} {}", command.name, command.summary)
        } else {
            format!("/{}", command.name)
        };
        frame.render_widget(
            Paragraph::new(label).style(super::panels::menu_style(index == state.slash_selection)),
            row,
        );
        hits.push(HitRegion {
            area: row,
            target: HitTarget::Action(Action::ExecuteCommand(command.kind)),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend, style::Modifier};

    #[test]
    fn palette_uses_shared_floating_geometry_without_covering_the_composer() {
        for (width, height) in [(40, 12), (80, 24), (100, 24), (160, 40)] {
            let mut state = UiState::default();
            state.orphan_draft = "/".into();
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let mut snapshot = None;
            terminal
                .draw(|frame| snapshot = Some(crate::view::render(frame, &state)))
                .unwrap();
            let snapshot = snapshot.unwrap();
            let composer = snapshot.layout.composer.expect("composer");
            let palette = snapshot
                .hit_regions()
                .iter()
                .find(|hit| hit.target == HitTarget::Capture)
                .expect("slash panel")
                .area;
            assert_eq!((palette.x, palette.width), (composer.x, composer.width));
            assert!(palette.bottom() < composer.y);
            assert!(palette.y > snapshot.layout.screen.y);
        }
    }

    #[test]
    fn slash_commands_use_the_shared_panel_shell_and_neutral_selection() {
        let mut state = UiState::default();
        state.orphan_draft = "/".into();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let mut snapshot = None;
        terminal
            .draw(|frame| snapshot = Some(crate::view::render(frame, &state)))
            .unwrap();

        let plan = snapshot.unwrap();
        let area = plan
            .hit_regions()
            .iter()
            .find(|hit| hit.target == HitTarget::Capture)
            .expect("slash panel")
            .area;
        let buffer = terminal.backend().buffer();
        let title = (area.y..area.bottom())
            .find_map(|y| {
                let row = (area.x..area.right())
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>();
                row.find("Commands").map(|x| (area.x + x as u16, y))
            })
            .expect("Commands title");
        let marker = &buffer[(area.x, title.1)];
        assert_eq!(marker.bg, theme::FOCUS_SURFACE);
        assert_ne!(marker.bg, theme::FOCUS_MARK);
        assert!(buffer[title].modifier.contains(Modifier::BOLD));

        let selected = plan
            .hit_regions()
            .iter()
            .find(|hit| matches!(hit.target, HitTarget::Action(Action::ExecuteCommand(_))))
            .expect("selected command row")
            .area;
        for x in selected.x..selected.right() {
            assert_eq!(buffer[(x, selected.y)].bg, theme::SELECTED);
            assert_ne!(buffer[(x, selected.y)].bg, theme::FOCUS_MARK);
        }
        assert!(
            plan.hit_regions()
                .iter()
                .any(|hit| hit.target == HitTarget::Capture && hit.area == area)
        );
    }

    #[test]
    fn empty_search_keeps_the_surface_open_and_captures_pointer_input() {
        let mut state = UiState::default();
        state.orphan_draft = "/does-not-exist".into();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let mut snapshot = None;
        terminal
            .draw(|frame| snapshot = Some(crate::view::render(frame, &state)))
            .unwrap();
        let snapshot = snapshot.unwrap();
        let area = snapshot
            .hit_regions()
            .iter()
            .find(|hit| hit.target == HitTarget::Capture)
            .expect("slash panel")
            .area;
        let screen = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(screen.contains("No matching commands"));
        assert_eq!(snapshot.hit(area.x + 1, area.y), Some(HitTarget::Capture));
    }
}
