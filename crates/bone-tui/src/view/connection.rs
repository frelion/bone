//! The connection steps belong to the model picker and never own chat drafts.
use super::{ACCENT, DANGER, INK, INPUT, MUTED, single_line_external};
use crate::{
    layout::{HitRegion, HitTarget, LayoutPlan},
    state::{ConnectionKind, Panel, SetupField, UiState},
};
use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    widgets::{Block, Clear, Paragraph, Wrap},
};

pub(super) fn render(frame: &mut Frame<'_>, plan: &mut LayoutPlan, state: &UiState) {
    let surface = plan.composer.unwrap_or(plan.screen);
    let height = state
        .connection_form
        .as_ref()
        .map_or(8, |form| form.fields().len() as u16 + 6)
        .min(plan.screen.height.saturating_sub(2));
    let area = Rect::new(
        surface.x,
        surface.y.saturating_sub(height + 1).max(plan.screen.y + 1),
        surface.width,
        height,
    );
    frame.render_widget(Clear, area);
    frame.render_widget(Block::default().style(Style::default().bg(INPUT)), area);
    let x = area.x + 2;
    let width = area.width.saturating_sub(4);
    let title = if matches!(state.panel, Some(Panel::ModelAdd)) {
        "Models / Add connection"
    } else {
        state
            .connection_form
            .as_ref()
            .map_or("Models / Connection", |form| form.kind.label())
    };
    frame.render_widget(
        Paragraph::new(title).style(Style::default().fg(INK)),
        Rect::new(x, area.y, width, 1),
    );
    let back = Rect::new(x, area.bottom().saturating_sub(1), width, 1);
    frame.render_widget(
        Paragraph::new("esc back").style(Style::default().fg(MUTED)),
        back,
    );
    plan.hit_regions.push(HitRegion {
        area: back,
        target: HitTarget::Back,
    });
    if matches!(state.panel, Some(Panel::ModelAdd)) {
        for (index, kind) in ConnectionKind::ALL.iter().enumerate() {
            let row = Rect::new(x, area.y + 2 + index as u16, width, 1);
            frame.render_widget(
                Paragraph::new(kind.label())
                    .style(super::panels::menu_style(state.panel_selection == index)),
                row,
            );
            plan.hit_regions.push(HitRegion {
                area: row,
                target: HitTarget::ConnectionKind(index),
            });
        }
        return;
    }
    let Some(form) = &state.connection_form else {
        return;
    };
    let hint = state
        .status
        .as_deref()
        .unwrap_or("tab fields · ctrl+u clear field");
    frame.render_widget(
        Paragraph::new(single_line_external(hint))
            .wrap(Wrap { trim: false })
            .style(Style::default().fg(if state.status.is_some() {
                DANGER
            } else {
                MUTED
            })),
        Rect::new(x, area.y + 1, width, 2),
    );
    for (index, field) in form.fields().iter().enumerate() {
        let row = Rect::new(x, area.y + 3 + index as u16, width, 1);
        let (label, value, placeholder) = match field {
            SetupField::Label => ("Name", form.label.as_str(), "connection name"),
            SetupField::BaseUrl => ("URL", form.base_url.as_str(), "official URL if blank"),
            SetupField::Key => (
                "Key",
                if form.key.as_str().is_empty() {
                    ""
                } else {
                    "********"
                },
                if form.key_was_sent {
                    "Re-enter API key"
                } else if form.existing.is_some() {
                    "blank: keep key unchanged"
                } else {
                    "API key required"
                },
            ),
            SetupField::Model => ("Model", form.model.as_str(), "model ID"),
        };
        let selected = form.field == *field;
        frame.render_widget(
            Paragraph::new(label).style(Style::default().fg(if selected { ACCENT } else { MUTED })),
            Rect::new(row.x, row.y, 6.min(width), 1),
        );
        let input = Rect::new(row.x + 6.min(width), row.y, width.saturating_sub(6), 1);
        let (text, cursor) =
            super::panels::input_query(value, input.width.saturating_add(2), placeholder);
        let text = text.strip_prefix("> ").unwrap_or(&text);
        let cursor = cursor.saturating_sub(2).min(input.width.saturating_sub(1));
        frame.render_widget(
            Paragraph::new(text).style(
                Style::default()
                    .fg(if value.is_empty() { MUTED } else { INK })
                    .bg(INPUT),
            ),
            input,
        );
        if selected && !form.saving && input.width > 2 {
            frame.set_cursor_position((input.x + cursor, input.y));
        }
        plan.hit_regions.push(HitRegion {
            area: row,
            target: HitTarget::SetupField(*field),
        });
    }
    let action = Rect::new(x, area.bottom().saturating_sub(2), width, 1);
    let label = if form.saving {
        "Saving…"
    } else if form.kind == ConnectionKind::ChatGptSubscription {
        "enter save & authorize"
    } else {
        "enter save · tab next field"
    };
    frame.render_widget(
        Paragraph::new(label).style(Style::default().fg(ACCENT)),
        action,
    );
    if !form.saving {
        plan.hit_regions.push(HitRegion {
            area: action,
            target: HitTarget::SaveConnection,
        });
    }
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;
    use crate::state::{ConnectionForm, SecretText};
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn connection_form_masks_key_and_keeps_all_fields_and_actions_inside_small_screens() {
        for (width, height) in [(40, 12), (80, 24), (120, 30), (160, 40)] {
            let mut form = ConnectionForm::new(ConnectionKind::OpenAiResponses);
            form.key = SecretText::from("not-a-real-key-KEEP-PRIVATE".to_owned());
            form.base_url = "https://example.invalid/long/address/for/viewport/checking".into();
            form.field = SetupField::Key;
            let mut state = UiState::default();
            state.panel = Some(Panel::ModelSetup);
            state.connection_form = Some(form);
            state.orphan_draft = "untouched chat draft".into();
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let mut layout = None;
            terminal
                .draw(|frame| layout = Some(crate::view::render(frame, &state)))
                .unwrap();
            let buffer = terminal.backend().buffer();
            let rendered = buffer
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            assert!(!rendered.contains("KEEP-PRIVATE"));
            assert!(!format!("{state:?}").contains("KEEP-PRIVATE"));
            assert!(rendered.contains("********"));
            let layout = layout.unwrap();
            for field in [
                SetupField::Label,
                SetupField::BaseUrl,
                SetupField::Key,
                SetupField::Model,
            ] {
                let hit = layout
                    .hit_regions
                    .iter()
                    .find(|hit| hit.target == HitTarget::SetupField(field))
                    .unwrap();
                assert!(hit.area.right() <= width && hit.area.bottom() <= height);
                assert_eq!(
                    layout.hit(hit.area.x, hit.area.y),
                    Some(HitTarget::SetupField(field))
                );
                let field_text: String = (hit.area.x..hit.area.right())
                    .map(|x| buffer[(x, hit.area.y)].symbol())
                    .collect();
                assert!(
                    !field_text.contains('>'),
                    "fields do not repeat prompt glyphs: {field_text}"
                );
                assert_eq!(buffer[(hit.area.x + 6, hit.area.y)].bg, INPUT);
            }
            assert!(
                layout
                    .hit_regions
                    .iter()
                    .any(|hit| hit.target == HitTarget::SaveConnection)
            );
            assert_eq!(state.orphan_draft, "untouched chat draft");
        }
    }

    #[test]
    fn failed_key_save_prompts_reentry_instead_of_suggesting_blank() {
        let mut form = ConnectionForm::edit(
            &bone_app::Profile::new(
                bone_app::ProfileId::new("test-api").unwrap(),
                "test",
                bone_app::EndpointConfig::OpenAiResponses { base_url: None },
            )
            .unwrap(),
            String::new(),
        );
        form.key_was_sent = true;
        let mut state = UiState::default();
        state.panel = Some(Panel::ModelSetup);
        state.connection_form = Some(form);
        let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
        terminal
            .draw(|frame| {
                crate::view::render(frame, &state);
            })
            .unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Re-enter API key"));
        assert!(!rendered.contains("keep key"));
    }

    #[test]
    fn all_connection_types_are_visible_and_clickable_at_minimum_size() {
        let mut state = UiState::default();
        state.panel = Some(Panel::ModelAdd);
        let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
        terminal
            .draw(|frame| {
                let layout = crate::view::render(frame, &state);
                for index in 0..4 {
                    let region = layout
                        .hit_regions
                        .iter()
                        .find(|r| r.target == HitTarget::ConnectionKind(index))
                        .unwrap();
                    assert_eq!(
                        layout.hit(region.area.x, region.area.y),
                        Some(HitTarget::ConnectionKind(index))
                    );
                }
            })
            .unwrap();
    }
}
