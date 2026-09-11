//! The connection steps belong to the model picker and never own chat drafts.
use super::{DANGER, INFO, INK, INPUT, MUTED, SELECTED, single_line_external};
use crate::{
    layout::{HitRegion, HitTarget, LayoutPlan},
    state::{ConnectionKind, Panel, SetupField, UiState},
    ui::{caret, focus, interaction::HitMap, theme},
};
use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    widgets::{Block, Clear, Paragraph, Wrap},
};

pub(super) fn render(frame: &mut Frame<'_>, plan: &LayoutPlan, hits: &mut HitMap, state: &UiState) {
    let surface = plan.composer.unwrap_or(plan.screen);
    let spacious = crate::layout::comfortable(plan.screen);
    let stride: u16 = if spacious { 2 } else { 1 };
    let inset = u16::from(spacious);
    let height = state
        .connection_form
        .as_ref()
        .map_or(if spacious { 13 } else { 8 }, |form| {
            form.fields().len() as u16 * stride + 6 + inset * 3
        })
        .min(plan.screen.height.saturating_sub(2));
    let area = Rect::new(
        surface.x,
        surface.y.saturating_sub(height + 1).max(plan.screen.y + 1),
        surface.width,
        height,
    );
    frame.render_widget(Clear, area);
    frame.render_widget(Block::default().style(theme::surface(INPUT)), area);
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
    let title_band = Rect::new(area.x, area.y + inset, area.width, 1);
    let title_background =
        focus::paint_header(frame, title_band, state, focus::Region::Panel, INPUT);
    frame.render_widget(
        Paragraph::new(title).style(theme::label_on(INK, title_background)),
        Rect::new(x, title_band.y, width, 1),
    );
    let back = Rect::new(x, area.bottom().saturating_sub(1 + inset), width, 1);
    frame.render_widget(
        Paragraph::new("esc back").style(Style::default().fg(MUTED)),
        back,
    );
    hits.push(HitRegion {
        area: back,
        target: HitTarget::Back,
    });
    if matches!(state.panel, Some(Panel::ModelAdd)) {
        for (index, kind) in ConnectionKind::ALL.iter().enumerate() {
            let row = Rect::new(
                x,
                area.y + 2 + inset * 2 + index as u16 * stride,
                width,
                stride,
            );
            frame.render_widget(
                Paragraph::new(kind.label())
                    .style(super::panels::menu_style(state.panel_selection == index)),
                row,
            );
            hits.push(HitRegion {
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
        Rect::new(x, area.y + 1 + inset, width, 2),
    );
    for (index, field) in form.fields().iter().enumerate() {
        let row = Rect::new(x, area.y + 3 + inset + index as u16 * stride, width, stride);
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
        let row_background = if selected { SELECTED } else { INPUT };
        frame.render_widget(
            Block::default().style(theme::surface(row_background)),
            Rect::new(row.x, row.y, row.width, 1),
        );
        frame.render_widget(
            Paragraph::new(label).style(if selected {
                theme::label_on(INK, row_background)
            } else {
                theme::body_on(MUTED, row_background)
            }),
            Rect::new(row.x, row.y, 8.min(width), 1),
        );
        let input = Rect::new(row.x + 8.min(width), row.y, width.saturating_sub(8), 1);
        let (text, cursor) =
            super::panels::input_query(value, input.width.saturating_add(2), placeholder);
        let text = text.strip_prefix("> ").unwrap_or(&text);
        let cursor = cursor.saturating_sub(2).min(input.width.saturating_sub(1));
        frame.render_widget(
            Paragraph::new(text).style(theme::body_on(
                if value.is_empty() { MUTED } else { INK },
                row_background,
            )),
            input,
        );
        if selected && !form.saving && input.width > 2 {
            caret::place(frame, (input.x + cursor, input.y), state.caret_visible);
        }
        hits.push(HitRegion {
            area: row,
            target: HitTarget::SetupField(*field),
        });
    }
    let action = Rect::new(x, area.bottom().saturating_sub(2 + inset), width, 1);
    let label = if form.saving {
        "Saving…"
    } else if form.kind == ConnectionKind::ChatGptSubscription {
        "enter save & authorize"
    } else {
        "enter save · tab next field"
    };
    let action_style = if form.saving {
        theme::label(INFO)
    } else {
        theme::label(INK)
    };
    frame.render_widget(Paragraph::new(label).style(action_style), action);
    if !form.saving {
        hits.push(HitRegion {
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
                    .hit_regions()
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
                assert_eq!(
                    buffer[(hit.area.x + 6, hit.area.y)].bg,
                    if field == SetupField::Key {
                        SELECTED
                    } else {
                        INPUT
                    }
                );
            }
            assert!(
                layout
                    .hit_regions()
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
                        .hit_regions()
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
