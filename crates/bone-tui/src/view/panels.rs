use super::single_line_external;
#[cfg(test)]
use crate::state::ModelPanel;
use crate::{
    layout::{HitRegion, HitTarget, LayoutPlan, floating_menu_stride, floating_panel_area},
    state::{Action, ModelOperationKind, ModelScreen, Panel, UiState},
    ui::{
        interaction::HitMap,
        theme::{self, INFO, INK, INPUT, MUTED},
    },
};
use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    text::{Line, Span},
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
    let title_background = theme::FOCUS_SURFACE;
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
    if let Panel::Reader(reader) = panel {
        let area = plan
            .extension_blank
            .or(plan.conversation)
            .unwrap_or(plan.screen);
        let metrics = super::reader::render(
            frame,
            area,
            &reader.content,
            state.selected_ui().and_then(|ui| ui.snapshot.as_deref()),
            reader.scroll,
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
    if let Panel::Models(models) = panel
        && matches!(
            models.screen,
            ModelScreen::Add { .. } | ModelScreen::Advanced { .. } | ModelScreen::Setup(_)
        )
    {
        super::connection::render(frame, plan, hits, state, models);
        return;
    }

    let surface = plan.composer.unwrap_or(plan.screen);
    let spacious = crate::layout::comfortable(plan.screen);
    let stride = floating_menu_stride(plan.screen);
    let (height, title) = match panel {
        Panel::Help => (16, "Keyboard help"),
        Panel::Objects(objects) => (
            objects.choices.len().clamp(1, 12) as u16 * stride + 3,
            "Tasks & tools · enter open",
        ),
        Panel::Models(models) => match &models.screen {
            ModelScreen::List { .. } => (
                (models.row_count() * usize::from(stride)
                    + state.model_configuration_summary().lines().count()
                    + 4
                    + if state.status.is_some() { 2 } else { 0 })
                .clamp(5, if spacious { 22 } else { 14 }) as u16,
                "Choose model",
            ),
            ModelScreen::Reasoning { model, .. } => (
                (models.reasoning_efforts(*model).len() * usize::from(stride) + 6)
                    .clamp(7, if spacious { 20 } else { 13 }) as u16,
                "Choose reasoning depth",
            ),
            ModelScreen::Manage { .. } => (
                (models.profiles.len().max(1) * usize::from(stride) + 4)
                    .clamp(5, if spacious { 22 } else { 14 }) as u16,
                "Manage connections",
            ),
            ModelScreen::Login { .. } => (10, "Sign in to ChatGPT"),
            ModelScreen::Add { .. } | ModelScreen::Advanced { .. } | ModelScreen::Setup(_) => {
                return;
            }
        },
        Panel::Reader(_) => return,
    };
    let area = floating_panel_area(plan.screen, surface, height);
    let shell = render_shell(frame, plan.screen, area, title);
    let mut inner = shell.inner;
    hits.push(HitRegion {
        area: shell.back,
        target: HitTarget::Action(Action::Escape),
    });

    match panel {
        Panel::Objects(objects) => {
            if let Some(error) = &state.status {
                frame.render_widget(
                    Paragraph::new(single_line_external(error.text()))
                        .style(Style::default().fg(theme::DANGER)),
                    Rect::new(inner.x, inner.y, inner.width, 1),
                );
                inner.y = inner.y.saturating_add(1);
                inner.height = inner.height.saturating_sub(1);
            }
            if objects.choices.is_empty() {
                frame.render_widget(
                    Paragraph::new("No loaded tasks or tool results")
                        .style(Style::default().fg(MUTED)),
                    inner,
                );
            } else {
                let start = objects
                    .selected
                    .saturating_sub(usize::from(inner.height / stride).saturating_sub(1));
                for (index, (_, label)) in objects
                    .choices
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
                            .style(menu_style(index == objects.selected)),
                        row,
                    );
                    hits.push(HitRegion {
                        area: row,
                        target: HitTarget::Action(Action::SelectObject(index)),
                    });
                }
            }
        }
        Panel::Models(models) => match &models.screen {
            ModelScreen::List { selected } => {
                if let Some(status) = &state.status {
                    let height = 2.min(inner.height.saturating_sub(1));
                    frame.render_widget(
                        Paragraph::new(single_line_external(status.text()))
                            .wrap(Wrap { trim: false })
                            .style(Style::default().fg(if status.is_error() {
                                theme::DANGER
                            } else {
                                INFO
                            })),
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
                if let Some(operation) = state
                    .model_operation
                    .filter(|operation| operation.session == models.session)
                {
                    let label = match operation.kind {
                        ModelOperationKind::Load => "Loading models…",
                        ModelOperationKind::Apply => "Switching model…",
                    };
                    frame.render_widget(
                        Paragraph::new(label).style(Style::default().fg(MUTED)),
                        inner,
                    );
                } else {
                    let action_stride = if spacious && inner.height >= 5 { 2 } else { 1 };
                    let action_height = (1 + action_stride * 2).min(inner.height);
                    let model_area = Rect::new(
                        inner.x,
                        inner.y,
                        inner.width,
                        inner.height.saturating_sub(action_height),
                    );
                    let choices_area = model_area;
                    let capacity = usize::from(choices_area.height / stride);
                    let cursor = (*selected).min(models.choices.len().saturating_sub(1));
                    let start = cursor.saturating_sub(capacity.saturating_sub(1));
                    for (index, choice) in
                        models.choices.iter().enumerate().skip(start).take(capacity)
                    {
                        let row = Rect::new(
                            choices_area.x,
                            choices_area.y + (index - start) as u16 * stride,
                            choices_area.width,
                            stride,
                        );
                        render_model_choice(
                            frame,
                            row,
                            state,
                            choice,
                            index == *selected,
                            spacious,
                        );
                        hits.push(HitRegion {
                            area: row,
                            target: HitTarget::Action(Action::SelectModel(index)),
                        });
                    }
                    if action_height > 0 {
                        let action_y = inner.bottom().saturating_sub(action_height);
                        frame.render_widget(
                            Paragraph::new("Connections").style(Style::default().fg(MUTED)),
                            Rect::new(inner.x, action_y, inner.width, 1),
                        );
                        for (offset, label) in ["+ Add account or API…", "Manage connections…"]
                            .into_iter()
                            .enumerate()
                        {
                            let index = models.choices.len() + offset;
                            let row = Rect::new(
                                inner.x,
                                action_y + 1 + offset as u16 * action_stride,
                                inner.width,
                                action_stride,
                            );
                            frame.render_widget(
                                Paragraph::new(label).style(model_menu_style(index == *selected)),
                                row,
                            );
                            hits.push(HitRegion {
                                area: row,
                                target: HitTarget::Action(Action::SelectModel(index)),
                            });
                        }
                    }
                }
            }
            ModelScreen::Reasoning { model, selected } => {
                let Some(choice) = models.choices.get(*model) else {
                    return;
                };
                let preset = models.preset(*model);
                let configured = state
                    .model_facts
                    .as_ref()
                    .and_then(|facts| facts.saved.as_ref().ok())
                    .filter(|resolved| {
                        resolved.selection.profile == choice.selection.profile
                            && resolved.selection.model == choice.selection.model
                    })
                    .and_then(|resolved| crate::state::model_effort(&resolved.selection));
                frame.render_widget(
                    Paragraph::new(Line::from(vec![
                        Span::styled(&choice.label, theme::label(INK)),
                        Span::styled(
                            format!("  {}", choice.profile_label),
                            Style::default().fg(MUTED),
                        ),
                    ])),
                    Rect::new(inner.x, inner.y, inner.width, 1),
                );
                let hint_y = inner.y.saturating_add(1);
                frame.render_widget(
                    Paragraph::new("How much time should this model spend reasoning?")
                        .style(Style::default().fg(MUTED)),
                    Rect::new(inner.x, hint_y, inner.width, 1),
                );
                let choices = models.reasoning_efforts(*model);
                let choices_y = hint_y.saturating_add(if spacious { 2 } else { 1 });
                let choices_height = inner.bottom().saturating_sub(choices_y);
                let capacity = usize::from(choices_height / stride);
                let cursor = (*selected).min(choices.len().saturating_sub(1));
                let start = cursor.saturating_sub(capacity.saturating_sub(1));
                for (index, effort) in choices.iter().enumerate().skip(start).take(capacity) {
                    let row = Rect::new(
                        inner.x,
                        choices_y + (index - start) as u16 * stride,
                        inner.width,
                        stride,
                    );
                    render_reasoning_choice(
                        frame,
                        row,
                        *effort,
                        index == *selected,
                        configured == Some(*effort),
                        preset.and_then(|preset| preset.default_reasoning) == Some(*effort),
                        spacious,
                    );
                    hits.push(HitRegion {
                        area: row,
                        target: HitTarget::Action(Action::SelectModel(index)),
                    });
                }
            }
            ModelScreen::Manage { selected } => {
                if models.profiles.is_empty() {
                    frame.render_widget(
                        Paragraph::new("No connections").style(Style::default().fg(MUTED)),
                        inner,
                    );
                } else {
                    let start = selected
                        .saturating_sub(usize::from(inner.height / stride).saturating_sub(1));
                    for (index, profile) in models
                        .profiles
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
                        let (label, note) = match &profile.endpoint {
                            bone_app::EndpointConfig::ChatGptSubscription => (
                                "ChatGPT account · sign in".to_owned(),
                                "Check or refresh sign-in",
                            ),
                            bone_app::EndpointConfig::OpenAiResponses { base_url: None } => {
                                ("OpenAI API · update key".to_owned(), "Keeps your model")
                            }
                            bone_app::EndpointConfig::AnthropicMessages { base_url: None } => {
                                ("Anthropic API · update key".to_owned(), "Keeps your model")
                            }
                            _ => (
                                format!("{} · edit", profile.label),
                                "Connection settings and model ID",
                            ),
                        };
                        let label = if spacious {
                            format!("{label}\n{note}")
                        } else {
                            label
                        };
                        frame.render_widget(
                            Paragraph::new(super::sanitize_external(&label))
                                .style(menu_style(index == *selected)),
                            row,
                        );
                        hits.push(HitRegion {
                            area: row,
                            target: HitTarget::Action(Action::SelectModel(index)),
                        });
                    }
                }
            }
            ModelScreen::Login { state: login, .. } => {
                let (text, retry) = match login {
                    bone_app::LoginState::Connecting => ("Connecting…".to_owned(), false),
                    bone_app::LoginState::DeviceCode {
                        verification_uri,
                        user_code,
                    } => (
                        format!(
                            "Open in your browser:\n{verification_uri}\n\nCode: {user_code}\nWaiting for sign-in…"
                        ),
                        false,
                    ),
                    bone_app::LoginState::Succeeded => {
                        ("Connected. Applying model…".to_owned(), false)
                    }
                    bone_app::LoginState::Failed { message } => {
                        (format!("Sign-in failed: {message}"), true)
                    }
                    bone_app::LoginState::Cancelled => ("Sign-in cancelled".to_owned(), true),
                };
                let body = Rect::new(
                    inner.x,
                    inner.y,
                    inner.width,
                    inner.height.saturating_sub(u16::from(retry)),
                );
                frame.render_widget(
                    Paragraph::new(super::sanitize_external(&text))
                        .wrap(Wrap { trim: false })
                        .style(Style::default().fg(INK)),
                    body,
                );
                if retry && inner.height > 0 {
                    let action = Rect::new(inner.x, inner.bottom() - 1, inner.width, 1);
                    frame.render_widget(
                        Paragraph::new("Retry sign-in").style(menu_style(true)),
                        action,
                    );
                    hits.push(HitRegion {
                        area: action,
                        target: HitTarget::Action(Action::ActivatePanel),
                    });
                }
            }
            ModelScreen::Add { .. } | ModelScreen::Advanced { .. } | ModelScreen::Setup(_) => {}
        },
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
                "/model        Models & connections".to_owned(),
                "/details      Latest task / tool".to_owned(),
            ];
            frame.render_widget(
                Paragraph::new(lines.map(Line::from).to_vec())
                    .style(Style::default().fg(INK))
                    .wrap(Wrap { trim: false }),
                inner,
            );
        }
        Panel::Reader(_) => {}
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{ModelScreen, ObjectPanel};
    use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, style::Modifier};

    fn models(screen: ModelScreen) -> Panel {
        let mut models = ModelPanel::new(None);
        models.screen = screen;
        Panel::Models(models)
    }

    fn objects(choices: Vec<(crate::state::reader::ReaderSource, String)>) -> Panel {
        Panel::Objects(ObjectPanel {
            session: bone_app::SessionId::new(),
            choices,
            selected: 0,
        })
    }

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
                models(ModelScreen::List { selected: 0 }),
                Panel::Help,
                models(ModelScreen::Login {
                    request: 1,
                    state: bone_app::LoginState::Connecting,
                }),
                objects(vec![]),
                objects(vec![(
                    crate::state::reader::ReaderSource::History(bone_app::SessionSeq(7)),
                    "Tool result 7".into(),
                )]),
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
        assert_eq!(mark.bg, theme::FOCUS_SURFACE);
        assert_ne!(mark.bg, theme::FOCUS_MARK);
        assert!(!mark.modifier.contains(Modifier::BOLD | Modifier::DIM));
        assert_eq!(buffer[(mark_x + 1, title_y)].bg, theme::FOCUS_SURFACE);
        let title = &buffer[(title_x, title_y)];
        assert_eq!(title.fg, INK);
        assert_eq!(title.bg, theme::FOCUS_SURFACE);
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

fn model_choice_label(state: &UiState, choice: &crate::state::ModelChoice) -> String {
    let mut parts = Vec::new();
    if let Some(marker) = state.model_selection_marker(&choice.selection) {
        parts.push(format!("✓ {marker}"));
    }
    parts.push(choice.label.clone());
    if choice.recommended {
        parts.push("Recommended".into());
    }
    parts.join(" · ")
}

fn render_model_choice(
    frame: &mut Frame<'_>,
    row: Rect,
    state: &UiState,
    choice: &crate::state::ModelChoice,
    selected: bool,
    spacious: bool,
) {
    let background = if selected { theme::PANEL } else { INPUT };
    let pointer = if selected { "› " } else { "  " };
    let pointer_tone = if selected { INFO } else { background };
    let title = model_choice_label(state, choice);
    let mut lines = vec![Line::from(vec![
        Span::styled(pointer, theme::label_on(pointer_tone, background)),
        Span::styled(
            super::single_line_external(&title),
            theme::label_on(INK, background),
        ),
    ])];
    if spacious {
        lines.push(Line::from(vec![
            Span::styled("  ", theme::body_on(MUTED, background)),
            Span::styled(
                super::single_line_external(&format!("{} · {}", choice.profile_label, choice.note)),
                theme::body_on(MUTED, background),
            ),
        ]));
    }
    frame.render_widget(Paragraph::new(lines).style(theme::surface(background)), row);
}

fn render_reasoning_choice(
    frame: &mut Frame<'_>,
    row: Rect,
    effort: bone_app::ReasoningEffort,
    selected: bool,
    configured: bool,
    recommended: bool,
    spacious: bool,
) {
    let background = if selected { theme::PANEL } else { INPUT };
    let (name, note) = reasoning_copy(effort);
    let mut title = name.to_owned();
    if configured {
        title.push_str(" · ✓ Current");
    } else if recommended {
        title.push_str(" · Recommended");
    }
    let pointer = if selected { "› " } else { "  " };
    let mut lines = vec![Line::from(vec![
        Span::styled(
            pointer,
            theme::label_on(if selected { INFO } else { background }, background),
        ),
        Span::styled(title, theme::label_on(INK, background)),
        Span::styled(
            format!("  {}", effort.as_str()),
            theme::body_on(MUTED, background),
        ),
    ])];
    if spacious {
        lines.push(Line::from(vec![
            Span::styled("  ", theme::body_on(MUTED, background)),
            Span::styled(note, theme::body_on(MUTED, background)),
        ]));
    }
    frame.render_widget(Paragraph::new(lines).style(theme::surface(background)), row);
}

fn reasoning_copy(effort: bone_app::ReasoningEffort) -> (&'static str, &'static str) {
    use bone_app::ReasoningEffort;
    match effort {
        ReasoningEffort::None => ("Instant", "Lowest latency, without deliberate reasoning"),
        ReasoningEffort::Minimal => ("Quick", "Minimal reasoning for very simple work"),
        ReasoningEffort::Low => ("Fast", "Light reasoning for straightforward work"),
        ReasoningEffort::Medium => ("Balanced", "A good default for everyday work"),
        ReasoningEffort::High => ("Deep", "More reasoning for complex tasks"),
        ReasoningEffort::Xhigh => ("Extra deep", "For hard problems; responses take longer"),
        ReasoningEffort::Max => ("Maximum", "Use the maximum reasoning available"),
    }
}

pub(super) fn menu_style(selected: bool) -> Style {
    if selected {
        theme::label_on(INK, theme::SELECTED)
    } else {
        theme::body_on(INK, INPUT)
    }
}

fn model_menu_style(selected: bool) -> Style {
    if selected {
        theme::label_on(INK, theme::PANEL)
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
            let mut models = ModelPanel::new(None);
            for index in 0..4 {
                models.choices.push(crate::state::ModelChoice {
                    selection: bone_app::ModelSelection::new(
                        bone_app::ProfileId::chatgpt(),
                        format!("model-{index}"),
                    )
                    .unwrap(),
                    profile_label: "ChatGPT".into(),
                    label: format!("Model {index}"),
                    note: "Test model".into(),
                    recommended: index == 0,
                });
            }
            models.profiles = vec![bone_app::Profile::chatgpt(); 5];
            let row_count = models.row_count();
            state.panel = Some(Panel::Models(models));
            for selected in 0..row_count {
                let Some(Panel::Models(models)) = &mut state.panel else {
                    unreachable!();
                };
                models.screen = ModelScreen::List { selected };
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
                assert_eq!(buffer[(hit.area.x, hit.area.y)].bg, theme::PANEL);
                assert_ne!(buffer[(hit.area.x, hit.area.y)].bg, theme::FOCUS_MARK);
                for y in 0..height {
                    let line: String = (0..width).map(|x| buffer[(x, y)].symbol()).collect();
                    if line.trim() == "Connections" {
                        assert!(!plan.hit_regions().iter().any(|hit| hit.area.y == y
                            && matches!(hit.target, HitTarget::Action(Action::SelectModel(_)))));
                    }
                }
            }
            let Some(Panel::Models(models)) = &mut state.panel else {
                unreachable!();
            };
            models.choices.clear();
            models.profiles.clear();
            models.screen = ModelScreen::List { selected: 0 };
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

    #[test]
    fn compact_error_state_keeps_the_selected_model_and_connection_actions_visible() {
        let profile = bone_app::Profile::chatgpt();
        let resolved = |model: &str| bone_app::ResolvedModel {
            selection: bone_app::ModelSelection::new(profile.id.clone(), model).unwrap(),
            profile: profile.clone(),
        };
        let mut state = UiState::default();
        state.model_facts = Some(crate::state::ModelFacts {
            saved: Ok(resolved("configured-model")),
            running: Some(resolved("current-model")),
        });
        state.status = Some("Model switch failed".into());
        let mut models = ModelPanel::new(None);
        models.choices = ["current-model", "configured-model"]
            .into_iter()
            .map(|model| crate::state::ModelChoice {
                selection: bone_app::ModelSelection::new(profile.id.clone(), model).unwrap(),
                profile_label: "ChatGPT".into(),
                label: model.into(),
                note: "Test model".into(),
                recommended: false,
            })
            .collect();
        models.screen = ModelScreen::List { selected: 1 };
        state.panel = Some(Panel::Models(models));
        let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
        let mut plan = None;
        terminal
            .draw(|frame| plan = Some(crate::view::render(frame, &state)))
            .unwrap();
        let plan = plan.unwrap();

        for index in [1, 2, 3] {
            assert!(
                plan.hit_regions()
                    .iter()
                    .any(|hit| { hit.target == HitTarget::Action(Action::SelectModel(index)) })
            );
        }
        let screen = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(screen.contains("✓ Configured"));
    }

    #[test]
    fn failed_login_has_a_pointer_retry_action() {
        let mut state = UiState::default();
        let mut models = ModelPanel::new(None);
        models.screen = ModelScreen::Login {
            request: 1,
            state: bone_app::LoginState::Failed {
                message: "authorization expired".into(),
            },
        };
        state.panel = Some(Panel::Models(models));
        let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
        let mut plan = None;
        terminal
            .draw(|frame| plan = Some(crate::view::render(frame, &state)))
            .unwrap();

        assert!(
            plan.unwrap()
                .hit_regions()
                .iter()
                .any(|hit| { hit.target == HitTarget::Action(Action::ActivatePanel) })
        );
    }
}
