use super::single_line_external;
use crate::{
    layout::{HitRegion, HitTarget, LayoutPlan, floating_menu_stride, floating_panel_area},
    state::{Action, Panel, UiState},
    ui::{
        interaction::HitMap,
        theme::{self, INK, INPUT, MUTED},
    },
};
use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    text::Line,
    widgets::{Block, Clear, Paragraph, Wrap},
};

const TITLE_INSET: u16 = 2;

pub(super) struct FloatingPanel {
    pub(super) inner: Rect,
    pub(super) back: Rect,
    pub(super) stride: u16,
}

/// Draw the one floating-panel shell used by dialogs and slash commands.
/// Callers own only the body rows and stable hit targets inside `inner`.
pub(super) fn render_shell(
    frame: &mut Frame<'_>,
    screen: Rect,
    area: Rect,
    title: &str,
) -> FloatingPanel {
    let spacious = crate::layout::comfortable(screen);
    let chrome: u16 = if spacious { 4 } else { 0 };
    frame.render_widget(Clear, area);
    frame.render_widget(Block::default().style(theme::surface(INPUT)), area);
    let inner = Rect::new(
        area.x + TITLE_INSET,
        area.y + 1 + chrome / 2,
        area.width.saturating_sub(TITLE_INSET * 2),
        area.height.saturating_sub(2 + chrome),
    );
    let title_y = area.y + u16::from(spacious);
    let title_background = theme::SELECTED;
    frame.render_widget(
        Block::default().style(theme::surface(title_background)),
        Rect::new(area.x, title_y, area.width, 1),
    );
    frame.render_widget(
        Paragraph::new(title).style(theme::label_on(INK, title_background)),
        Rect::new(inner.x, title_y, inner.width, 1),
    );
    if spacious {
        frame.render_widget(
            Paragraph::new("─".repeat(usize::from(inner.width)))
                .style(theme::body_on(theme::STRUCTURE, INPUT)),
            Rect::new(inner.x, area.y + 2, inner.width, 1),
        );
    }
    let back = Rect::new(
        inner.x,
        area.bottom() - 1 - u16::from(spacious),
        inner.width,
        1,
    );
    frame.render_widget(
        Paragraph::new("esc back").style(Style::default().fg(MUTED)),
        back,
    );
    FloatingPanel {
        inner,
        back,
        stride: floating_menu_stride(screen),
    }
}

pub(super) fn render(
    frame: &mut Frame<'_>,
    plan: &LayoutPlan,
    hits: &mut HitMap,
    reader_max_scroll: &mut usize,
    state: &UiState,
) {
    let Some(panel) = &state.panel else {
        return;
    };
    hits.clear();
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
        );
        *reader_max_scroll = metrics.max_scroll;
        hits.push(HitRegion {
            area: metrics.body,
            target: HitTarget::Reader,
        });
        hits.push(HitRegion {
            area: metrics.back,
            target: HitTarget::Action(Action::Escape),
        });
        return;
    }
    if matches!(panel, Panel::ModelAdd | Panel::ModelSetup) {
        super::connection::render(frame, plan, hits, state);
        return;
    }
    let surface = plan.composer.unwrap_or(plan.screen);
    let spacious = crate::layout::comfortable(plan.screen);
    let stride = floating_menu_stride(plan.screen);
    let height = match panel {
        Panel::Help => 16,
        Panel::Login => 10,
        Panel::Objects { choices, .. } => choices.len().clamp(1, 12) as u16 * stride + 3,
        Panel::Models => (state.model_row_count() * usize::from(stride)
            + state.model_configuration_summary().lines().count()
            + 4
            + if state.status.is_some() { 2 } else { 0 })
        .clamp(5, if spacious { 22 } else { 14 }) as u16,
        Panel::Reader(_) | Panel::ModelAdd | Panel::ModelSetup => unreachable!(),
    };
    let area = floating_panel_area(plan.screen, surface, height);
    let title = match panel {
        Panel::Models => "Models & connections",
        Panel::Objects { .. } => "Tasks & tools · enter open",
        Panel::Login => "Models / Account authorization",
        Panel::Help => "Keyboard help",
        Panel::Reader(_) | Panel::ModelAdd | Panel::ModelSetup => unreachable!(),
    };
    let shell = render_shell(frame, plan.screen, area, title);
    let mut inner = shell.inner;
    let back = shell.back;
    hits.push(HitRegion {
        area: back,
        target: HitTarget::Action(Action::Escape),
    });
    match panel {
        Panel::Objects { choices, .. } => {
            let mut inner = inner;
            if let Some(error) = &state.status {
                frame.render_widget(
                    Paragraph::new(single_line_external(error))
                        .style(Style::default().fg(theme::DANGER)),
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
                    .saturating_sub(usize::from(inner.height / stride).saturating_sub(1));
                for (index, (_, label)) in choices
                    .iter()
                    .enumerate()
                    .skip(start)
                    .take(usize::from(inner.height / stride))
                {
                    let row = Rect::new(
                        inner.x,
                        inner.y + (index - start) as u16 * stride,
                        inner.width,
                        stride,
                    );
                    frame.render_widget(
                        Paragraph::new(single_line_external(label))
                            .style(menu_style(index == state.panel_selection)),
                        row,
                    );
                    hits.push(HitRegion {
                        area: row,
                        target: HitTarget::Action(Action::SelectObject(index)),
                    });
                }
            }
        }
        Panel::Models => {
            if let Some(status) = &state.status {
                let height = 2.min(inner.height.saturating_sub(1));
                frame.render_widget(
                    Paragraph::new(single_line_external(status))
                        .wrap(Wrap { trim: false })
                        .style(Style::default().fg(theme::DANGER)),
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
                let start =
                    selected.saturating_sub(usize::from(inner.height / stride).saturating_sub(1));
                for (visual_index, item) in menu
                    .iter()
                    .enumerate()
                    .skip(start)
                    .take(usize::from(inner.height / stride))
                {
                    let row = Rect::new(
                        inner.x,
                        inner.y + (visual_index - start) as u16 * stride,
                        inner.width,
                        stride,
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
                    hits.push(HitRegion {
                        area: row,
                        target: HitTarget::Action(Action::SelectModel(index)),
                    });
                }
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
            let shift_enter = if state.terminal_capabilities.shift_enter_supported() {
                "Shift+Enter   New line in input".to_owned()
            } else {
                let reason = single_line_external(
                    state
                        .terminal_capabilities
                        .keyboard_limitation()
                        .unwrap_or("terminal cannot distinguish the modifier"),
                );
                format!("Shift+Enter   Unavailable · {}", reason)
            };
            let lines = [
                "Ctrl+←/→/↑/↓  Spatial focus".to_owned(),
                "Enter         Submit / choose".to_owned(),
                shift_enter,
                "Ctrl+C        Clear focused input".to_owned(),
                "Ctrl+D        Save drafts and quit".to_owned(),
                "Esc           Back, then stop".to_owned(),
                "Ctrl+Z/Y      Undo / redo in input".to_owned(),
                "/model        Model configuration".to_owned(),
                "/details      Latest task / tool".to_owned(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, style::Modifier};

    fn find_text(buffer: &Buffer, needle: &str) -> Option<(u16, u16)> {
        for y in buffer.area.y..buffer.area.bottom() {
            let line = (buffer.area.x..buffer.area.right())
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>();
            if let Some(x) = line.find(needle) {
                return Some((buffer.area.x + x as u16, y));
            }
        }
        None
    }

    #[test]
    fn editable_panels_keep_cursor_and_actions_inside_small_screens() {
        for (width, height) in [(40, 12), (80, 24), (140, 24), (160, 40)] {
            for panel in [
                Panel::Models,
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
                let mut state = UiState::default();
                state.panel = Some(panel);
                state.status = Some("Invalid value".into());
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                let mut plan = None;
                terminal
                    .draw(|frame| {
                        plan = Some(crate::view::render(frame, &state));
                    })
                    .unwrap();
                let plan = plan.unwrap();
                for region in plan.hit_regions() {
                    assert!(
                        region.area.right() <= width && region.area.bottom() <= height,
                        "{region:?} in {width}x{height}"
                    );
                    assert!(!matches!(
                        region.target,
                        HitTarget::Composer | HitTarget::Action(Action::ClickSubmit)
                    ));
                }
                let back = plan
                    .hit_regions()
                    .iter()
                    .find(|region| region.target == HitTarget::Action(Action::Escape))
                    .unwrap();
                assert_eq!(
                    plan.hit(back.area.x, back.area.y),
                    Some(HitTarget::Action(Action::Escape))
                );
            }
        }
    }

    #[test]
    fn panel_title_uses_a_neutral_active_surface_without_orange() {
        let mut state = UiState::default();
        state.panel = Some(Panel::Help);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| {
                crate::view::render(frame, &state);
            })
            .unwrap();

        let buffer = terminal.backend().buffer();
        let (title_x, title_y) =
            find_text(buffer, "Keyboard help").expect("panel title is visible");
        let mark_x = title_x - TITLE_INSET;
        let mark = &buffer[(mark_x, title_y)];
        assert_eq!(mark.symbol(), " ");
        assert_eq!(mark.bg, theme::SELECTED);
        assert_ne!(mark.bg, theme::FOCUS_MARK);
        assert!(!mark.modifier.contains(Modifier::BOLD | Modifier::DIM));
        assert_eq!(buffer[(mark_x + 1, title_y)].bg, theme::SELECTED);
        let title = &buffer[(title_x, title_y)];
        assert_eq!(title.fg, INK);
        assert_eq!(title.bg, theme::SELECTED);
        assert!(title.modifier.contains(Modifier::BOLD));
        assert!(
            buffer
                .content()
                .iter()
                .all(|cell| cell.fg != theme::FOCUS_MARK && cell.bg != theme::FOCUS_MARK)
        );
    }

    #[test]
    fn help_names_only_the_declared_focus_and_editor_shortcuts() {
        let mut state = UiState::default();
        state.panel = Some(Panel::Help);
        let mut terminal = Terminal::new(TestBackend::new(80, 30)).unwrap();
        terminal
            .draw(|frame| {
                crate::view::render(frame, &state);
            })
            .unwrap();
        let screen = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        for shortcut in [
            "Ctrl+←/→/↑/↓",
            "Enter",
            "Shift+Enter",
            "Ctrl+C",
            "Ctrl+D",
            "Ctrl+Z/Y",
        ] {
            assert!(screen.contains(shortcut), "missing shortcut: {shortcut}");
        }
        for alias in ["Ctrl+J", "Alt+Enter", "Ctrl+P"] {
            assert!(!screen.contains(alias), "invented shortcut shown: {alias}");
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
    if selected {
        theme::label_on(INK, theme::SELECTED)
    } else {
        theme::body_on(INK, INPUT)
    }
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
                    .hit_regions()
                    .iter()
                    .find(|hit| hit.target == HitTarget::Action(Action::SelectModel(selected)))
                    .expect("selected action remains visible");
                assert_eq!(
                    plan.hit(hit.area.x, hit.area.y),
                    Some(HitTarget::Action(Action::SelectModel(selected)))
                );
                let buffer = terminal.backend().buffer();
                assert_eq!(buffer[(hit.area.x, hit.area.y)].bg, theme::SELECTED);
                assert_eq!(buffer[(hit.area.x, hit.area.y)].fg, INK);
                assert_ne!(buffer[(hit.area.x, hit.area.y)].bg, theme::FOCUS_MARK);
                for y in 0..height {
                    let line: String = (0..width).map(|x| buffer[(x, y)].symbol()).collect();
                    if line.contains("Manage connections") {
                        assert!(!plan.hit_regions().iter().any(|hit| hit.area.y == y
                            && matches!(hit.target, HitTarget::Action(Action::SelectModel(_)))));
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
                        plan.hit_regions()
                            .iter()
                            .any(|hit| hit.target == HitTarget::Action(Action::SelectModel(0)))
                    );
                })
                .unwrap();
        }
    }
}
