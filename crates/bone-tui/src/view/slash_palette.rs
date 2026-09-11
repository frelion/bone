use ratatui::{Frame, layout::Rect, widgets::Paragraph};

use crate::{
    layout::{HitRegion, HitTarget},
    state::{CommandSpec, UiState},
    ui::{interaction::HitMap, theme},
    view::MUTED,
};

pub(super) fn render(
    frame: &mut Frame<'_>,
    screen: Rect,
    area: Rect,
    hits: &mut HitMap,
    state: &UiState,
    matches: &[&CommandSpec],
) {
    let shell = super::panels::render_shell(frame, screen, area, "Commands", true);

    // The complete surface captures pointer input so its title, padding and
    // blank rows cannot fall through to the conversation underneath it.
    hits.push(HitRegion {
        area: shell.area,
        target: HitTarget::CommandPalette,
    });
    hits.push(HitRegion {
        area: shell.back,
        target: HitTarget::Back,
    });

    let capacity = usize::from(shell.inner.height / shell.stride).max(1);
    let start = state
        .slash_selection
        .saturating_sub(capacity.saturating_sub(1));
    if matches.is_empty() {
        frame.render_widget(
            Paragraph::new("No matching commands").style(theme::body_on(MUTED, theme::INPUT)),
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
            target: HitTarget::SlashCommand(command.kind),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend, style::Modifier};

    #[test]
    fn slash_commands_use_the_shared_panel_shell_and_neutral_selection() {
        let mut state = UiState::default();
        state.orphan_draft = "/".into();
        state.orphan_cursor = 1;
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let mut snapshot = None;
        terminal
            .draw(|frame| snapshot = Some(crate::view::render(frame, &state)))
            .unwrap();

        let plan = snapshot.unwrap();
        let area = plan.layout.slash_palette.expect("slash panel");
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
        assert_eq!(marker.bg, theme::SELECTED);
        assert_ne!(marker.bg, theme::FOCUS_MARK);
        assert!(buffer[title].modifier.contains(Modifier::BOLD));

        let selected = plan
            .hit_regions()
            .iter()
            .find(|hit| matches!(hit.target, HitTarget::SlashCommand(_)))
            .expect("selected command row")
            .area;
        for x in selected.x..selected.right() {
            assert_eq!(buffer[(x, selected.y)].bg, theme::SELECTED);
            assert_ne!(buffer[(x, selected.y)].bg, theme::FOCUS_MARK);
        }
        assert!(
            plan.hit_regions()
                .iter()
                .any(|hit| hit.target == HitTarget::CommandPalette && hit.area == area)
        );
    }

    #[test]
    fn empty_search_keeps_the_surface_open_and_captures_pointer_input() {
        let mut state = UiState::default();
        state.orphan_draft = "/does-not-exist".into();
        state.orphan_cursor = state.orphan_draft.len();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let mut snapshot = None;
        terminal
            .draw(|frame| snapshot = Some(crate::view::render(frame, &state)))
            .unwrap();
        let snapshot = snapshot.unwrap();
        let area = snapshot.layout.slash_palette.expect("slash panel");
        let screen = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(screen.contains("No matching commands"));
        assert_eq!(
            snapshot.hit(area.x + 1, area.y),
            Some(HitTarget::CommandPalette)
        );
    }
}
