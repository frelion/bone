//! The connection steps belong to the model picker and never own chat drafts.
use super::single_line_external;
use crate::{
    layout::{ClickRegion, ClickTarget, LayoutPlan},
    state::{Action, ConnectionKind, ModelPanel, ModelScreen, SetupField, UiState},
    ui::{
        caret,
        interaction::SurfaceHits,
        theme::{self, DANGER, INFO, INK, INPUT, MUTED, SELECTED},
    },
};
use ratatui::{
    Frame,
    layout::Rect,
    widgets::{Block, Paragraph},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub(super) fn render(
    frame: &mut Frame<'_>,
    plan: &LayoutPlan,
    hits: &mut SurfaceHits,
    state: &UiState,
    models: &ModelPanel,
    editor: Option<Rect>,
) -> Option<Rect> {
    let desired = match &models.screen {
        ModelScreen::Setup(form) => form.fields().len() as u16 * 2 + 7,
        ModelScreen::Add { .. } | ModelScreen::Advanced { .. } => 13,
        _ => return None,
    };
    let area = plan.overlay_area(desired, editor);
    if area.height < 3 {
        return None;
    }
    let title = match &models.screen {
        ModelScreen::Add { .. } => "Models / Add connection",
        ModelScreen::Advanced { .. } => "Models / Custom connection",
        ModelScreen::Setup(form) => form.kind.label(),
        _ => return None,
    };
    let shell = super::overlay::render_shell(frame, plan.screen, area, title);
    frame.render_widget(
        Paragraph::new(super::overlay::keyboard_hint(state)).style(theme::body(MUTED)),
        shell.back,
    );
    hits.push(ClickRegion {
        area: shell.back,
        target: ClickTarget::Action(Action::OverlayBack),
    });
    hits.push(ClickRegion {
        area: shell.close,
        target: ClickTarget::Action(Action::CloseOverlay),
    });
    let mut body = shell.inner;
    let stride = shell.stride;
    if let ModelScreen::Add { selected } = &models.screen {
        let has_openai = models
            .profiles
            .iter()
            .any(|profile| profile.id.as_str() == "openai");
        let has_anthropic = models
            .profiles
            .iter()
            .any(|profile| profile.id.as_str() == "anthropic");
        let options = [
            (
                "ChatGPT · sign in",
                "Uses your subscription · opens a browser",
            ),
            (
                if has_openai {
                    "OpenAI API · update key"
                } else {
                    "OpenAI API · connect"
                },
                if has_openai {
                    "Keeps the model you already selected"
                } else {
                    "API key only · uses the recommended model"
                },
            ),
            (
                if has_anthropic {
                    "Anthropic API · update key"
                } else {
                    "Anthropic API · connect"
                },
                if has_anthropic {
                    "Keeps the model you already selected"
                } else {
                    "API key only · uses the recommended model"
                },
            ),
            ("Custom / advanced", "Compatible service URL and model ID"),
        ];
        let capacity = usize::from(body.height / stride);
        let start = selected.saturating_sub(capacity.saturating_sub(1));
        for (index, (label, note)) in options.iter().enumerate().skip(start).take(capacity) {
            let row = Rect::new(
                body.x,
                body.y + (index - start) as u16 * stride,
                body.width,
                stride,
            );
            frame.render_widget(
                Paragraph::new(if stride > 1 {
                    format!("{label}\n{note}")
                } else {
                    (*label).into()
                })
                .style(super::overlay::menu_style(*selected == index)),
                row,
            );
            hits.push(ClickRegion {
                area: row,
                target: ClickTarget::Action(Action::ChooseConnection(index)),
            });
        }
        hits.push_scroll(area, crate::ui::interaction::ScrollTarget::Overlay);
        return Some(area);
    }
    if let ModelScreen::Advanced { selected } = &models.screen {
        let capacity = usize::from(body.height / stride);
        let start = selected.saturating_sub(capacity.saturating_sub(1));
        for (index, kind) in ConnectionKind::ADVANCED
            .iter()
            .enumerate()
            .skip(start)
            .take(capacity)
        {
            let row = Rect::new(
                body.x,
                body.y + (index - start) as u16 * stride,
                body.width,
                stride,
            );
            frame.render_widget(
                Paragraph::new(kind.label()).style(super::overlay::menu_style(*selected == index)),
                row,
            );
            hits.push(ClickRegion {
                area: row,
                target: ClickTarget::Action(Action::ChooseConnection(index)),
            });
        }
        hits.push_scroll(area, crate::ui::interaction::ScrollTarget::Overlay);
        return Some(area);
    }
    let ModelScreen::Setup(form) = &models.screen else {
        return Some(area);
    };
    if body.height >= 4 {
        let hint = state
            .status_text()
            .unwrap_or("tab fields · ctrl+u clear field");
        frame.render_widget(
            Paragraph::new(single_line_external(hint)).style(theme::body(match &state.status {
                Some(status) if status.is_error() => DANGER,
                Some(_) => INFO,
                None => MUTED,
            })),
            Rect::new(body.x, body.y, body.width, 1),
        );
        body.y += 1;
        body.height -= 1;
    }
    let fields = form.fields();
    // Fields and Save share one bounded viewport; wheel never selects a keyboard field.
    let capacity = usize::from(body.height);
    let max = (fields.len() + 1).saturating_sub(capacity);
    let mut start = state.overlay_scroll.min(max);
    if state.keyboard.is_overlay() {
        let active = fields
            .iter()
            .position(|field| *field == form.field)
            .unwrap_or(0);
        start = start
            .min(active)
            .max(active.saturating_sub(capacity.saturating_sub(1)));
    }
    hits.push_scroll(
        area,
        crate::ui::interaction::ScrollTarget::OverlayContent { max },
    );
    for (index, field) in fields.iter().enumerate().skip(start).take(capacity) {
        let row = Rect::new(body.x, body.y + (index - start) as u16, body.width, 1);
        let (label, value, placeholder) = match field {
            SetupField::Label => ("Name", form.label.as_str(), "connection name"),
            SetupField::BaseUrl => ("URL", form.base_url.as_str(), "https://… (required)"),
            SetupField::Key => (
                "Key",
                if form.key.as_str().is_empty() {
                    ""
                } else {
                    "********"
                },
                if form.key_was_sent {
                    "Re-enter API key"
                } else if form.edits_existing_connection() {
                    "blank: keep key unchanged"
                } else {
                    "API key required"
                },
            ),
            SetupField::Model => ("Model", form.model.as_str(), "model ID"),
        };
        let selected = state.keyboard.is_overlay() && form.field == *field;
        let background = if selected { SELECTED } else { INPUT };
        frame.render_widget(Block::default().style(theme::surface(background)), row);
        frame.render_widget(
            Paragraph::new(label).style(if selected {
                theme::label_on(INK, background)
            } else {
                theme::body_on(MUTED, background)
            }),
            Rect::new(row.x, row.y, 8.min(row.width), 1),
        );
        let input = Rect::new(
            row.x + 8.min(row.width),
            row.y,
            row.width.saturating_sub(8),
            1,
        );
        let (text, cursor) = input_query(value, input.width, placeholder);
        frame.render_widget(
            Paragraph::new(text).style(theme::body_on(
                if value.is_empty() { MUTED } else { INK },
                background,
            )),
            input,
        );
        if selected && form.pending_request.is_none() && input.width > 2 {
            caret::place(frame, (input.x + cursor, input.y), state.caret_visible);
        }
    }
    let save_index = fields.len();
    if save_index >= start && save_index < start + capacity {
        let row = Rect::new(body.x, body.y + (save_index - start) as u16, body.width, 1);
        let label = if form.pending_request.is_some() {
            "Saving…"
        } else if form.changes_model() {
            "enter save & use changed model"
        } else if form.edits_existing_connection() {
            "enter save connection"
        } else if form.kind.advanced() {
            "enter save & use model · tab next field"
        } else {
            "enter save & use recommended model"
        };
        frame.render_widget(
            Paragraph::new(label).style(theme::label(if form.pending_request.is_some() {
                INFO
            } else {
                INK
            })),
            row,
        );
        if form.pending_request.is_none() {
            hits.push(ClickRegion {
                area: row,
                target: ClickTarget::Action(Action::SaveConnection),
            });
        }
    }
    Some(area)
}

// Setup fields append at the end. Keep that position visible without splitting
// a wide or combining grapheme.
fn input_query(value: &str, width: u16, placeholder: &str) -> (String, u16) {
    if value.is_empty() {
        return (placeholder.into(), 0);
    }
    let clean = single_line_external(value);
    let available = usize::from(width.saturating_sub(1));
    let mut cells = 0;
    let mut suffix = Vec::new();
    for grapheme in clean.graphemes(true).rev() {
        let size = UnicodeWidthStr::width(grapheme);
        if cells + size > available {
            break;
        }
        suffix.push(grapheme);
        cells += size;
    }
    suffix.reverse();
    (suffix.concat(), cells as u16)
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;
    use crate::state::{ConnectionForm, ModelPanel, ModelScreen, Overlay, SecretText};
    use ratatui::{Terminal, backend::TestBackend};

    fn connection_panel(screen: ModelScreen) -> Overlay {
        let mut models = ModelPanel::new(None);
        models.screen = screen;
        Overlay::Models(models)
    }

    #[test]
    fn long_fields_keep_the_grapheme_tail_and_caret_inside_the_input() {
        for value in [
            "https://example.invalid/a-long-path/TAIL",
            "long-prefix-中e\u{301}🙂尾",
        ] {
            let (text, cursor) = input_query(value, 9, "unused");
            assert!(value.ends_with(&text));
            assert!(text.ends_with(value.graphemes(true).next_back().unwrap()));
            assert_eq!(usize::from(cursor), UnicodeWidthStr::width(text.as_str()));
            assert!(cursor < 9);
        }
        assert_eq!(input_query("", 9, "placeholder"), ("placeholder".into(), 0));
    }

    #[test]
    fn connection_form_masks_key_and_keeps_all_fields_and_actions_inside_small_screens() {
        for (width, height) in [(40, 12), (80, 24), (120, 30), (160, 40)] {
            let mut form = ConnectionForm::new(ConnectionKind::CustomOpenAiResponses);
            form.key = SecretText::from("not-a-real-key-KEEP-PRIVATE".to_owned());
            form.base_url = "https://example.invalid/long/address/for/viewport/checking".into();
            form.field = SetupField::Key;
            let mut state = UiState::default();
            state.overlay = Some(connection_panel(ModelScreen::Setup(Box::new(form))));
            state.enter_overlay();
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
            assert!(
                layout
                    .hit_regions()
                    .into_iter()
                    .any(|hit| hit.target == ClickTarget::Action(Action::SaveConnection))
            );
            assert_eq!(state.orphan_draft.text(), "untouched chat draft");
        }
    }

    #[test]
    fn failed_key_save_prompts_reentry_instead_of_suggesting_blank() {
        let mut form = ConnectionForm::edit_selection(
            &bone_app::Profile::new(
                bone_app::ProfileId::new("test-api").unwrap(),
                "test",
                bone_app::EndpointConfig::OpenAiResponses { base_url: None },
            )
            .unwrap(),
            None,
        )
        .unwrap();
        form.key_was_sent = true;
        let mut state = UiState::default();
        state.overlay = Some(connection_panel(ModelScreen::Setup(Box::new(form))));
        state.enter_overlay();
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
    fn pending_save_shows_progress_without_an_action_or_caret() {
        let mut form = ConnectionForm::new(ConnectionKind::OpenAiApi);
        form.pending_request = Some(7);
        let mut state = UiState::default();
        state.overlay = Some(connection_panel(ModelScreen::Setup(Box::new(form))));
        state.enter_overlay();
        let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
        let mut layout = None;

        terminal
            .draw(|frame| layout = Some(crate::view::render(frame, &state)))
            .unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Saving…"));
        assert!(
            !layout
                .unwrap()
                .hit_regions()
                .into_iter()
                .any(|hit| { hit.target == ClickTarget::Action(Action::SaveConnection) })
        );
        assert!(!state.blinking_caret_active());
    }

    #[test]
    fn all_connection_types_are_visible_and_clickable_at_minimum_size() {
        let mut state = UiState::default();
        state.overlay = Some(connection_panel(ModelScreen::Add { selected: 0 }));
        let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
        terminal
            .draw(|frame| {
                let layout = crate::view::render(frame, &state);
                for index in 0..4 {
                    let region = layout
                        .hit_regions()
                        .into_iter()
                        .find(|r| r.target == ClickTarget::Action(Action::ChooseConnection(index)))
                        .unwrap();
                    assert_eq!(
                        layout.hit(region.area.x, region.area.y),
                        Some(ClickTarget::Action(Action::ChooseConnection(index)))
                    );
                }
            })
            .unwrap();
    }
}
