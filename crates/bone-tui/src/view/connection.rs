//! The connection steps belong to the model picker and never own chat drafts.
use super::single_line_external;
use crate::{
    layout::{ClickRegion, ClickTarget, LayoutPlan},
    state::{Action, KindChoice, ModelPanel, ModelScreen, SetupField, UiState},
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
        ModelScreen::ModelForm(_) => 10,
        ModelScreen::Kind { .. } => KindChoice::ALL.len() as u16 * 2 + 5,
        ModelScreen::ModelInput { .. } => 8,
        ModelScreen::ConfirmDelete { .. } => 9,
        _ => return None,
    };
    let area = plan.overlay_area(desired, editor);
    if area.height < 3 {
        return None;
    }
    let title = match &models.screen {
        ModelScreen::Kind { .. } => "Models / Add connection",
        ModelScreen::ModelInput { .. } => "Models / Enter model ID",
        ModelScreen::ConfirmDelete { .. } => "Models / Delete connection",
        ModelScreen::Setup(form) => form.kind.label(),
        ModelScreen::ModelForm(_) => "Models / Remove model",
        _ => return None,
    };
    let shell = super::overlay::render_shell(frame, plan.screen, area, title);
    let hint = state
        .status_text()
        .unwrap_or_else(|| screen_hint(state, models));
    frame.render_widget(
        Paragraph::new(single_line_external(hint)).style(theme::body(match &state.status {
            Some(status) if status.is_error() => DANGER,
            Some(_) => INFO,
            None if models.busy(state.model_operation) => INFO,
            None => MUTED,
        })),
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
    if let ModelScreen::Kind { selected } = &models.screen {
        let capacity = usize::from(body.height / stride);
        let start = selected.saturating_sub(capacity.saturating_sub(1));
        for (index, choice) in KindChoice::ALL
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
            // A kind that is already saved opens its tab instead of asking for
            // the same details twice, so say so before the user picks it.
            let note = if models
                .profiles
                .iter()
                .any(|profile| choice.already_connected(profile))
            {
                "Already connected · opens its tab"
            } else {
                choice.note()
            };
            frame.render_widget(
                Paragraph::new(if stride > 1 {
                    format!("{}\n{note}", choice.label())
                } else {
                    choice.label().to_owned()
                })
                .style(super::overlay::menu_style(
                    *selected == index && state.keyboard.is_overlay(),
                )),
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
    if let ModelScreen::ModelInput { value } = &models.screen {
        let input = Rect::new(body.x, body.y, body.width, 1);
        let selected = state.keyboard.is_overlay();
        let background = if selected { SELECTED } else { INPUT };
        frame.render_widget(Block::default().style(theme::surface(background)), input);
        frame.render_widget(
            Paragraph::new("Model").style(if selected {
                theme::label_on(INK, background)
            } else {
                theme::body_on(MUTED, background)
            }),
            Rect::new(input.x, input.y, 8.min(input.width), 1),
        );
        let field = Rect::new(
            input.x + 8.min(input.width),
            input.y,
            input.width.saturating_sub(8),
            1,
        );
        let (text, cursor) = input_query(value, field.width, "model ID");
        frame.render_widget(
            Paragraph::new(text).style(theme::body_on(
                if value.is_empty() { MUTED } else { INK },
                background,
            )),
            field,
        );
        if selected && field.width > 2 {
            caret::place(frame, (field.x + cursor, field.y), state.caret_visible);
        }
        if body.height > 1 {
            let apply = Rect::new(body.x, body.y + 1, body.width, 1);
            frame.render_widget(
                Paragraph::new("enter apply model · esc back").style(theme::label(INK)),
                apply,
            );
            hits.push(ClickRegion {
                area: apply,
                target: ClickTarget::Action(Action::ActivatePanel),
            });
        }
        hits.push_scroll(
            area,
            crate::ui::interaction::ScrollTarget::OverlayContent { max: 0 },
        );
        return Some(area);
    }
    if matches!(models.screen, ModelScreen::ConfirmDelete { .. }) {
        let label = models.tab_profile().map_or_else(
            || "this connection".to_owned(),
            |profile| profile.label.clone(),
        );
        let question = Rect::new(body.x, body.y, body.width, 1);
        frame.render_widget(
            Paragraph::new(single_line_external(&format!("Delete {label}?")))
                .style(theme::label(INK)),
            question,
        );
        if body.height > 1 {
            frame.render_widget(
                Paragraph::new("Its saved API key is deleted with it.").style(theme::body(MUTED)),
                Rect::new(body.x, body.y + 1, body.width, 1),
            );
        }
        // The two answers share the bottom of the box. A box with a single row
        // only has room for one of them; "y" stays available on the keyboard.
        let cancel = Rect::new(
            body.x,
            body.y + body.height.saturating_sub(1),
            body.width,
            1,
        );
        let delete = Rect::new(
            body.x,
            body.y + body.height.saturating_sub(2),
            body.width,
            1,
        );
        if body.height > 1 {
            frame.render_widget(
                Paragraph::new("[y] delete connection").style(theme::label(DANGER)),
                delete,
            );
            hits.push(ClickRegion {
                area: delete,
                target: ClickTarget::Action(Action::ConfirmDeleteConnection),
            });
        }
        frame.render_widget(
            Paragraph::new("[n] cancel").style(theme::label(INK)),
            cancel,
        );
        hits.push(ClickRegion {
            area: cancel,
            target: ClickTarget::Action(Action::Escape),
        });
        hits.push_scroll(
            area,
            crate::ui::interaction::ScrollTarget::OverlayContent { max: 0 },
        );
        return Some(area);
    }
    let is_model_form = matches!(&models.screen, ModelScreen::ModelForm(_));
    if is_model_form {
        let ModelScreen::ModelForm(form) = &models.screen else {
            unreachable!()
        };
        if body.height >= 4 {
            frame.render_widget(
                Paragraph::new(single_line_external(
                    state
                        .status_text()
                        .unwrap_or("enter remove model · esc cancel"),
                ))
                .style(theme::body(MUTED)),
                Rect::new(body.x, body.y, body.width, 1),
            );
            body.y += 1;
            body.height -= 1;
        }
        let row = Rect::new(body.x, body.y, body.width, 1);
        let selected = state.keyboard.is_overlay();
        let background = if selected { SELECTED } else { INPUT };
        frame.render_widget(Block::default().style(theme::surface(background)), row);
        frame.render_widget(
            Paragraph::new("Model").style(if selected {
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
        let (text, _) = input_query(&form.model, input.width, "model ID");
        frame.render_widget(
            Paragraph::new(text).style(theme::body_on(
                if form.model.is_empty() { MUTED } else { INK },
                background,
            )),
            input,
        );
        let save = Rect::new(body.x, body.y + 1, body.width, 1);
        frame.render_widget(
            Paragraph::new(if form.pending_request.is_some() {
                "Removing…"
            } else {
                "enter remove model"
            })
            .style(theme::label(if form.pending_request.is_some() {
                INFO
            } else {
                INK
            })),
            save,
        );
        if form.pending_request.is_none() {
            hits.push(ClickRegion {
                area: save,
                target: ClickTarget::Action(Action::SaveConnection),
            });
        }
        hits.push_scroll(
            area,
            crate::ui::interaction::ScrollTarget::OverlayContent { max: 0 },
        );
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
            SetupField::BaseUrl => ("URL", form.base_url.as_str(), "http(s)://… (required)"),
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
                    "optional · blank for local services"
                },
            ),
            SetupField::Model => ("Model", form.model.as_str(), "model ID (optional)"),
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
        } else if form.edits_existing_connection() {
            "enter save connection"
        } else if form.kind.advanced() {
            "enter save connection · tab next field"
        } else {
            "enter save connection"
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

/// The keys each sub-screen owns, named on the shell's escape row.
fn screen_hint(state: &UiState, models: &ModelPanel) -> &'static str {
    match &models.screen {
        ModelScreen::Kind { .. } => "↑↓ choose kind · enter next · esc back",
        ModelScreen::ModelInput { .. } => "type or paste a model ID · enter apply · esc back",
        ModelScreen::ConfirmDelete { .. } => "y delete · n cancel",
        _ if models.busy(state.model_operation) => "Applying model…",
        _ => super::overlay::keyboard_hint(state),
    }
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
    use crate::state::{
        ConnectionForm, ConnectionKind, KindChoice, ModelPanel, ModelScreen, Overlay, SecretText,
    };
    use ratatui::{Terminal, backend::TestBackend};

    fn connection_panel(screen: ModelScreen) -> Overlay {
        connection_panel_for(Vec::new(), screen)
    }

    fn connection_panel_for(profiles: Vec<bone_app::Profile>, screen: ModelScreen) -> Overlay {
        let mut models = ModelPanel::new(None);
        models.profiles = profiles;
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
    fn the_selected_connection_kind_stays_painted_at_minimum_size() {
        let mut state = UiState::default();
        state.overlay = Some(connection_panel(ModelScreen::Kind { selected: 0 }));
        state.enter_overlay();
        let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
        for selected in 0..KindChoice::ALL.len() {
            let Some(Overlay::Models(models)) = state.overlay.as_mut() else {
                unreachable!()
            };
            models.screen = ModelScreen::Kind { selected };
            let mut layout = None;
            terminal
                .draw(|frame| layout = Some(crate::view::render(frame, &state)))
                .unwrap();
            let layout = layout.unwrap();
            let region = layout
                .hit_regions()
                .into_iter()
                .find(|r| r.target == ClickTarget::Action(Action::ChooseConnection(selected)))
                .expect("the selected kind stays inside the bounded viewport");
            assert_eq!(
                layout.hit(region.area.x, region.area.y),
                Some(ClickTarget::Action(Action::ChooseConnection(selected)))
            );
        }
    }

    #[test]
    fn the_kind_picker_names_a_connection_that_already_has_a_tab() {
        let mut state = UiState::default();
        state.overlay = Some(connection_panel_for(
            vec![bone_app::Profile::chatgpt()],
            ModelScreen::Kind { selected: 0 },
        ));
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
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
        assert!(rendered.contains("Already connected"));
    }

    #[test]
    fn the_manual_model_editor_shows_its_value_and_an_apply_action() {
        let mut state = UiState::default();
        state.overlay = Some(connection_panel(ModelScreen::ModelInput {
            value: "gpt-5.6-terra".into(),
        }));
        state.enter_overlay();
        let mut terminal = Terminal::new(TestBackend::new(60, 16)).unwrap();
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
        assert!(rendered.contains("gpt-5.6-terra"));
        assert!(rendered.contains("enter apply model"));
        assert!(state.blinking_caret_active());
        assert!(
            layout
                .unwrap()
                .hit_regions()
                .into_iter()
                .any(|hit| hit.target == ClickTarget::Action(Action::ActivatePanel))
        );
    }

    #[test]
    fn the_delete_confirmation_names_the_connection_and_offers_y_and_n() {
        let mut state = UiState::default();
        state.overlay = Some(connection_panel_for(
            vec![bone_app::Profile::chatgpt()],
            ModelScreen::ConfirmDelete { selected: 0 },
        ));
        let mut terminal = Terminal::new(TestBackend::new(60, 16)).unwrap();
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
        assert!(rendered.contains("Delete ChatGPT subscription?"));
        assert!(rendered.contains("[y] delete connection"));
        assert!(rendered.contains("[n] cancel"));

        let layout = layout.unwrap();
        for target in [
            ClickTarget::Action(Action::ConfirmDeleteConnection),
            ClickTarget::Action(Action::Escape),
        ] {
            let region = layout
                .hit_regions()
                .into_iter()
                .find(|hit| hit.target == target)
                .unwrap();
            assert_eq!(layout.hit(region.area.x, region.area.y), Some(target));
        }
    }

    #[test]
    fn the_removal_form_only_offers_removal() {
        let mut profile = bone_app::Profile::new(
            bone_app::ProfileId::new("custom").unwrap(),
            "Custom",
            bone_app::EndpointConfig::OpenAiResponses {
                base_url: Some("https://example.invalid/v1".into()),
            },
        )
        .unwrap();
        profile.add_model("model-a").unwrap();
        let mut state = UiState::default();
        state.overlay = Some(connection_panel(ModelScreen::ModelForm(Box::new(
            crate::state::ModelForm::remove(profile, "model-a"),
        ))));
        state.enter_overlay();
        let mut terminal = Terminal::new(TestBackend::new(60, 16)).unwrap();
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
        assert!(rendered.contains("Remove model"));
        assert!(rendered.contains("enter remove model"));
        assert!(!rendered.contains("catalog only"));
        assert!(
            layout
                .unwrap()
                .hit_regions()
                .into_iter()
                .any(|hit| hit.target == ClickTarget::Action(Action::SaveConnection))
        );
    }
}
