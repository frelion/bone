use super::{ACCENT, INK, INPUT, MUTED, PANEL, single_line_external};
use crate::{
    layout::{HitRegion, HitTarget, LayoutPlan},
    state::{COMMANDS, Panel, UiState},
};
use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    text::Line,
    widgets::{Block, Clear, Paragraph, Wrap},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub(super) fn render(frame: &mut Frame<'_>, plan: &mut LayoutPlan, state: &UiState) {
    let Some(panel) = &state.panel else {
        return;
    };
    plan.hit_regions.clear();
    if let Panel::Reader(content) = panel {
        let area = plan
            .extension_blank
            .or(plan.conversation)
            .unwrap_or(plan.screen);
        let metrics = super::reader::render(
            frame,
            area,
            content,
            state.selected_ui().and_then(|ui| ui.snapshot.as_deref()),
            state.panel_scroll,
            true,
        );
        plan.reader_max_scroll = metrics.max_scroll;
        plan.hit_regions.push(HitRegion {
            area: metrics.body,
            target: HitTarget::Reader,
        });
        plan.hit_regions.push(HitRegion {
            area: metrics.back,
            target: HitTarget::Back,
        });
        return;
    }
    if matches!(panel, Panel::ModelAdd | Panel::ModelSetup) {
        super::connection::render(frame, plan, state);
        return;
    }
    let surface = plan.composer.unwrap_or(plan.screen);
    let height = match panel {
        Panel::Help => 16,
        Panel::Login => 10,
        Panel::Rename => 7,
        Panel::Commands => COMMANDS.len() as u16 + 3,
        Panel::Objects { choices, .. } => choices.len().clamp(1, 12) as u16 + 3,
        Panel::Models => (state.model_row_count()
            + state.model_configuration_summary().lines().count()
            + 4
            + if state.status.is_some() { 2 } else { 0 })
        .clamp(5, 14) as u16,
        _ => 0,
    }
    .min(plan.screen.height.saturating_sub(2))
    .max(3);
    let area = Rect::new(
        surface.x,
        surface.y.saturating_sub(height + 1).max(plan.screen.y + 1),
        surface.width,
        height,
    );
    frame.render_widget(Clear, area);
    frame.render_widget(Block::default().style(Style::default().bg(INPUT)), area);
    let mut inner = Rect::new(
        area.x + 2,
        area.y + 1,
        area.width.saturating_sub(4),
        area.height.saturating_sub(2),
    );
    let title = match panel {
        Panel::Commands => "Commands",
        Panel::Models => "Models & connections",
        Panel::Rename => "Rename session",
        Panel::Objects { .. } => "Tasks & tools · enter open",
        Panel::Login => "Models / Account authorization",
        _ => "Keyboard help",
    };
    frame.render_widget(
        Paragraph::new(title).style(Style::default().fg(INK)),
        Rect::new(inner.x, area.y, inner.width, 1),
    );
    let back = Rect::new(inner.x, area.bottom() - 1, inner.width, 1);
    frame.render_widget(
        Paragraph::new("esc back").style(Style::default().fg(MUTED)),
        back,
    );
    plan.hit_regions.push(HitRegion {
        area: back,
        target: HitTarget::Back,
    });
    match panel {
        Panel::Objects { choices, .. } => {
            let mut inner = inner;
            if let Some(error) = &state.status {
                frame.render_widget(
                    Paragraph::new(single_line_external(error)).style(Style::default().fg(ACCENT)),
                    Rect::new(inner.x, inner.y, inner.width, 1),
                );
                inner.y = inner.y.saturating_add(1);
                inner.height = inner.height.saturating_sub(1);
            }
            if choices.is_empty() {
                frame.render_widget(
                    Paragraph::new("No loaded tasks or tool results")
                        .style(Style::default().fg(MUTED)),
                    inner,
                );
            } else {
                let start = state
                    .panel_selection
                    .saturating_sub(usize::from(inner.height).saturating_sub(1));
                for (index, (_, label)) in choices
                    .iter()
                    .enumerate()
                    .skip(start)
                    .take(usize::from(inner.height))
                {
                    let row = Rect::new(inner.x, inner.y + (index - start) as u16, inner.width, 1);
                    frame.render_widget(
                        Paragraph::new(single_line_external(label))
                            .style(menu_style(index == state.panel_selection)),
                        row,
                    );
                    plan.hit_regions.push(HitRegion {
                        area: row,
                        target: HitTarget::Object(index),
                    });
                }
            }
        }
        Panel::Commands => {
            let start = state
                .panel_selection
                .saturating_sub(usize::from(inner.height).saturating_sub(1));
            for (index, command) in COMMANDS
                .iter()
                .enumerate()
                .skip(start)
                .take(usize::from(inner.height))
            {
                let row = Rect::new(inner.x, inner.y + (index - start) as u16, inner.width, 1);
                let label = if inner.width >= 52 {
                    format!("/{:<12} {}", command.name, command.summary)
                } else {
                    format!("/{}", command.name)
                };
                frame.render_widget(
                    Paragraph::new(label).style(menu_style(index == state.panel_selection)),
                    row,
                );
                plan.hit_regions.push(HitRegion {
                    area: row,
                    target: HitTarget::SlashCommand(index),
                });
            }
        }
        Panel::Models => {
            if let Some(status) = &state.status {
                let height = 2.min(inner.height.saturating_sub(1));
                frame.render_widget(
                    Paragraph::new(single_line_external(status))
                        .wrap(Wrap { trim: false })
                        .style(Style::default().fg(ACCENT)),
                    Rect::new(inner.x, inner.y, inner.width, height),
                );
                inner.y += height;
                inner.height -= height;
            }
            let summary = state.model_configuration_summary();
            let rows = (summary.lines().count() as u16).min(inner.height.saturating_sub(1));
            if rows > 0 {
                frame.render_widget(
                    Paragraph::new(super::sanitize_external(&summary))
                        .style(Style::default().fg(MUTED)),
                    Rect::new(inner.x, inner.y, inner.width, rows),
                );
            }
            let inner = Rect::new(
                inner.x,
                inner.y + rows,
                inner.width,
                inner.height.saturating_sub(rows),
            );
            if state.models_loading {
                frame.render_widget(
                    Paragraph::new("Loading configuration…").style(Style::default().fg(MUTED)),
                    inner,
                );
            } else {
                let menu = model_menu_rows(state);
                let selected = menu.iter().position(|row| matches!(row, ModelMenuRow::Choice(index) if *index == state.panel_selection)).unwrap_or(0);
                let start = selected.saturating_sub(usize::from(inner.height).saturating_sub(1));
                for (visual_index, item) in menu
                    .iter()
                    .enumerate()
                    .skip(start)
                    .take(usize::from(inner.height))
                {
                    let row = Rect::new(
                        inner.x,
                        inner.y + (visual_index - start) as u16,
                        inner.width,
                        1,
                    );
                    let index = match item {
                        ModelMenuRow::Heading(label) => {
                            frame.render_widget(
                                Paragraph::new(*label).style(Style::default().fg(MUTED)),
                                row,
                            );
                            continue;
                        }
                        ModelMenuRow::Choice(index) => *index,
                    };
                    let label = if let Some(choice) = state.model_choices.get(index) {
                        model_choice_label(choice)
                    } else if let Some(profile) =
                        state.model_profiles.get(index - state.model_choices.len())
                    {
                        format!("Edit connection · {}", profile.label)
                    } else {
                        "+ Add model / connection".to_owned()
                    };
                    frame.render_widget(
                        Paragraph::new(single_line_external(&label))
                            .style(menu_style(index == state.panel_selection)),
                        row,
                    );
                    plan.hit_regions.push(HitRegion {
                        area: row,
                        target: HitTarget::Model(index),
                    });
                }
            }
        }

        Panel::Rename => {
            let (text, cursor) = input_query(&state.rename_input, inner.width, "session title");
            frame.render_widget(
                Paragraph::new(text).style(Style::default().fg(ACCENT)),
                Rect::new(inner.x, inner.y, inner.width, 1),
            );
            if inner.width > 2 {
                frame.set_cursor_position((inner.x + cursor, inner.y));
            }
            frame.render_widget(
                Paragraph::new("enter save title").style(Style::default().fg(MUTED)),
                Rect::new(inner.x, inner.y + 1, inner.width, 1),
            );
            if let Some(error) = &state.status {
                frame.render_widget(
                    Paragraph::new(single_line_external(error))
                        .wrap(Wrap { trim: false })
                        .style(Style::default().fg(super::DANGER)),
                    Rect::new(
                        inner.x,
                        inner.y + 2,
                        inner.width,
                        inner.height.saturating_sub(2),
                    ),
                );
            }
        }
        Panel::Login => {
            let text = match &state.login_state {
                bone_app::LoginState::Connecting => "Connecting…".to_owned(),
                bone_app::LoginState::DeviceCode {
                    verification_uri,
                    user_code,
                } => format!(
                    "Open in your browser:\n{verification_uri}\n\nCode: {user_code}\nWaiting for sign-in…"
                ),
                bone_app::LoginState::Succeeded => {
                    "Connected. Choose a model with /model, then retry saved input.".to_owned()
                }
                bone_app::LoginState::Failed { message } => format!("Sign-in failed: {message}"),
                bone_app::LoginState::Cancelled => "Sign-in cancelled".to_owned(),
            };
            frame.render_widget(
                Paragraph::new(super::sanitize_external(&text))
                    .wrap(Wrap { trim: false })
                    .style(Style::default().fg(INK)),
                inner,
            );
        }
        Panel::Help => {
            let lines = [
                "Ctrl+arrows   Move focus",
                "Enter         Submit / choose",
                "Alt+Enter     New line",
                "Ctrl+P        Commands",
                "/model        Model configuration",
                "/details      Latest task / tool",
                "Esc           Back, then stop",
                "Ctrl+Q        Save drafts and quit",
            ];
            frame.render_widget(
                Paragraph::new(lines.map(Line::from).to_vec())
                    .style(Style::default().fg(INK))
                    .wrap(Wrap { trim: false }),
                inner,
            );
        }
        Panel::Reader(_) | Panel::ModelAdd | Panel::ModelSetup => unreachable!(),
    }
}

// Inline panel editing currently appends at the end. Keep that insertion position
// visible without splitting a wide or combining grapheme at the left edge.
pub(super) fn input_query(value: &str, width: u16, placeholder: &str) -> (String, u16) {
    if value.is_empty() {
        return (format!("> {placeholder}"), 2.min(width.saturating_sub(1)));
    }
    let clean = single_line_external(value);
    let available = usize::from(width.saturating_sub(3));
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
    let truncated = suffix.iter().map(|part| part.len()).sum::<usize>() < clean.len();
    (
        format!("{} {}", if truncated { "…" } else { ">" }, suffix.concat()),
        (2 + cells as u16).min(width.saturating_sub(1)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn model_query_keeps_graphemes_and_insertion_cell_visible() {
        let (text, cursor) = input_query("very-long-profile 中文e\u{301}TAIL", 12, "");
        assert!(text.ends_with("e\u{301}TAIL"));
        assert_eq!(UnicodeWidthStr::width(text.as_str()), usize::from(cursor));
        assert!(cursor < 12);
        let (text, _) = input_query("prefix\u{1b}\nTAIL", 12, "");
        assert!(!text.chars().any(char::is_control));
    }

    #[test]
    fn editable_panels_keep_cursor_and_actions_inside_small_screens() {
        for (width, height) in [(40, 12), (80, 24), (140, 24), (160, 40)] {
            for panel in [
                Panel::Models,
                Panel::Rename,
                Panel::Commands,
                Panel::Help,
                Panel::Login,
                Panel::Objects {
                    session: bone_app::SessionId::new(),
                    choices: vec![],
                },
                Panel::Objects {
                    session: bone_app::SessionId::new(),
                    choices: vec![(
                        crate::state::reader::ReaderSource::History(bone_app::SessionSeq(7)),
                        "Tool result 7".into(),
                    )],
                },
            ] {
                let editable = matches!(panel, Panel::Rename);
                let mut state = UiState::default();
                state.panel = Some(panel);
                state.rename_input = format!("{}TAIL", "汉e\u{301}".repeat(40));
                state.status = Some("Invalid value".into());
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                let mut plan = None;
                terminal
                    .draw(|frame| {
                        plan = Some(crate::view::render(frame, &state));
                    })
                    .unwrap();
                let plan = plan.unwrap();
                for region in &plan.hit_regions {
                    assert!(
                        region.area.right() <= width && region.area.bottom() <= height,
                        "{region:?} in {width}x{height}"
                    );
                    assert!(!matches!(
                        region.target,
                        HitTarget::Composer | HitTarget::Submit
                    ));
                }
                let back = plan
                    .hit_regions
                    .iter()
                    .find(|region| region.target == HitTarget::Back)
                    .unwrap();
                assert_eq!(plan.hit(back.area.x, back.area.y), Some(HitTarget::Back));
                if editable {
                    let position = terminal.get_cursor_position().unwrap();
                    assert!(position.x < width && position.y < back.area.y);
                    let row: String = (0..width)
                        .map(|x| terminal.backend().buffer()[(x, position.y)].symbol())
                        .collect();
                    assert!(row.contains("TAIL"), "panel input tail hidden: {row}");
                    let screen: String = (0..height)
                        .flat_map(|y| (0..width).map(move |x| (x, y)))
                        .map(|position| terminal.backend().buffer()[position].symbol())
                        .collect();
                    assert!(screen.contains("Invalid value"), "panel error hidden");
                }
            }
        }
    }
}

fn model_choice_label(choice: &crate::state::ModelChoice) -> String {
    let options = choice
        .selection
        .options
        .as_ref()
        .map(|options| match options {
            bone_app::ModelOptions::OpenAiResponses { reasoning } => {
                serde_json::to_value(reasoning)
                    .ok()
                    .and_then(|value| {
                        value.as_object().map(|fields| {
                            fields
                                .iter()
                                .map(|(key, value)| {
                                    format!("{key} {}", value.as_str().unwrap_or_default())
                                })
                                .collect::<Vec<_>>()
                                .join(", ")
                        })
                    })
                    .unwrap_or_default()
            }
        })
        .unwrap_or_default();
    if options.is_empty() {
        format!("{} · {}", choice.selection.model, choice.profile_label)
    } else {
        format!(
            "[{options}] {} · {}",
            choice.selection.model, choice.profile_label
        )
    }
}

#[test]
fn same_model_with_distinct_reasoning_options_has_distinct_visible_labels() {
    let label = |effort| {
        let mut selection =
            bone_app::ModelSelection::new(bone_app::ProfileId::chatgpt(), "same-model").unwrap();
        selection.options = Some(
            serde_json::from_value(
                serde_json::json!({"type":"openai_responses","reasoning":{"effort":effort}}),
            )
            .unwrap(),
        );
        model_choice_label(&crate::state::ModelChoice {
            selection,
            profile_label: "account".into(),
        })
    };
    assert!(label("high").starts_with("[effort high]"));
    assert!(label("low").starts_with("[effort low]"));
}

/// A group heading never consumes an action index or receives a pointer target.
enum ModelMenuRow {
    Heading(&'static str),
    Choice(usize),
}
fn model_menu_rows(state: &UiState) -> Vec<ModelMenuRow> {
    let mut rows = Vec::with_capacity(state.model_row_count() + 2);
    if !state.model_choices.is_empty() {
        rows.push(ModelMenuRow::Heading("Available models"));
        rows.extend((0..state.model_choices.len()).map(ModelMenuRow::Choice));
    }
    if !rows.is_empty() {
        rows.push(ModelMenuRow::Heading(""));
    }
    rows.push(ModelMenuRow::Heading("Manage connections"));
    rows.extend((state.model_choices.len()..state.model_row_count()).map(ModelMenuRow::Choice));
    rows
}
pub(super) fn menu_style(selected: bool) -> Style {
    Style::default()
        .fg(if selected { PANEL } else { INK })
        .bg(if selected { ACCENT } else { INPUT })
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod grouped_menu_tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};
    #[test]
    fn grouping_keeps_every_model_action_reachable_and_headers_inert() {
        for (width, height) in [(40, 12), (80, 24), (160, 40)] {
            let mut state = UiState::default();
            state.panel = Some(Panel::Models);
            for index in 0..4 {
                state.model_choices.push(crate::state::ModelChoice {
                    selection: bone_app::ModelSelection::new(
                        bone_app::ProfileId::chatgpt(),
                        format!("model-{index}"),
                    )
                    .unwrap(),
                    profile_label: "ChatGPT".into(),
                });
            }
            state.model_profiles = vec![bone_app::Profile::chatgpt(); 5];
            for selected in 0..state.model_row_count() {
                state.panel_selection = selected;
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                let mut plan = None;
                terminal
                    .draw(|frame| plan = Some(crate::view::render(frame, &state)))
                    .unwrap();
                let plan = plan.unwrap();
                let hit = plan
                    .hit_regions
                    .iter()
                    .find(|hit| hit.target == HitTarget::Model(selected))
                    .expect("selected action remains visible");
                assert_eq!(
                    plan.hit(hit.area.x, hit.area.y),
                    Some(HitTarget::Model(selected))
                );
                let buffer = terminal.backend().buffer();
                assert_eq!(buffer[(hit.area.x, hit.area.y)].bg, ACCENT);
                assert_eq!(buffer[(hit.area.x, hit.area.y)].fg, PANEL);
                for y in 0..height {
                    let line: String = (0..width).map(|x| buffer[(x, y)].symbol()).collect();
                    if line.contains("Manage connections") {
                        assert!(!plan.hit_regions.iter().any(
                            |hit| hit.area.y == y && matches!(hit.target, HitTarget::Model(_))
                        ));
                    }
                }
            }
            state.model_choices.clear();
            state.model_profiles.clear();
            state.panel_selection = 0;
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| {
                    let plan = crate::view::render(frame, &state);
                    assert!(
                        plan.hit_regions
                            .iter()
                            .any(|hit| hit.target == HitTarget::Model(0))
                    );
                })
                .unwrap();
        }
    }
}
