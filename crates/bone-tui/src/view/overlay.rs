use super::single_line_external;
#[cfg(test)]
use crate::state::ModelPanel;
use crate::{
    layout::{ClickRegion, ClickTarget, LayoutPlan, floating_menu_stride},
    state::{Action, ModelOperationKind, ModelScreen, Overlay, UiState},
    ui::{
        interaction::SurfaceHits,
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

pub(super) fn keyboard_hint(state: &UiState) -> &'static str {
    if state.keyboard.is_overlay() {
        "↑↓ move · enter choose · f6 input"
    } else {
        "click or enter choose · f6 keyboard"
    }
}

pub(super) struct FloatingPanel {
    pub(super) inner: Rect,
    pub(super) back: Rect,
    pub(super) close: Rect,
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
    let spacious = crate::layout::comfortable(screen) && area.height >= 10;
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
        Rect::new(inner.x, title_y, inner.width.saturating_sub(2), 1),
    );
    let close = Rect::new(area.right().saturating_sub(3), title_y, 1, 1);
    frame.render_widget(
        Paragraph::new("×").style(theme::label_on(MUTED, title_background)),
        close,
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
        close,
        stride: if spacious { 2 } else { 1 },
    }
}

pub(super) fn render(
    frame: &mut Frame<'_>,
    plan: &LayoutPlan,
    hits: &mut SurfaceHits,
    editor: Option<Rect>,
    state: &UiState,
) -> Option<Rect> {
    let Some(panel) = &state.overlay else {
        return None;
    };
    if let Overlay::Models(models) = panel
        && matches!(
            models.screen,
            ModelScreen::Add { .. } | ModelScreen::Advanced { .. } | ModelScreen::Setup(_)
        )
    {
        return super::connection::render(frame, plan, hits, state, models, editor);
    }

    let spacious = crate::layout::comfortable(plan.screen);
    let stride = floating_menu_stride(plan.screen);
    let (height, title) = match panel {
        Overlay::Help => (16, "Keyboard help"),
        Overlay::Objects(objects) => (
            objects.choices.len().clamp(1, 12) as u16 * stride + 3,
            "Tasks & tools · enter open",
        ),
        Overlay::Models(models) => match &models.screen {
            ModelScreen::List { .. } => (
                (models.row_count() * usize::from(stride)
                    + state.model_configuration_summary().lines().count()
                    + 4
                    + if state.status.is_some() { 2 } else { 0 })
                .clamp(5, if spacious { 22 } else { 14 }) as u16,
                "Choose model",
            ),
            ModelScreen::Manage { .. } => (
                (models.profiles.len().max(1) * usize::from(stride) + 4)
                    .clamp(5, if spacious { 22 } else { 14 }) as u16,
                "Manage connections",
            ),
            ModelScreen::Login { .. } => (10, "Sign in to ChatGPT"),
            ModelScreen::Add { .. } | ModelScreen::Advanced { .. } | ModelScreen::Setup(_) => {
                return None;
            }
        },
    };
    let area = plan.overlay_area(height, editor);
    if area.height < 3 {
        return None;
    }
    hits.push_scroll(area, crate::ui::interaction::ScrollTarget::Overlay);
    let shell = render_shell(frame, plan.screen, area, title);
    let busy = match panel {
        Overlay::Models(models) => state
            .model_operation
            .filter(|operation| operation.session == models.session)
            .map(|operation| match operation.kind {
                ModelOperationKind::Load => "loading models…",
                ModelOperationKind::Apply => "switching model…",
            }),
        Overlay::Objects(_) | Overlay::Help => None,
    };
    frame.render_widget(
        Paragraph::new(busy.unwrap_or_else(|| keyboard_hint(state)))
            .style(Style::default().fg(if busy.is_some() { INFO } else { MUTED })),
        shell.back,
    );
    let stride = shell.stride;
    hits.push(ClickRegion {
        area: shell.close,
        target: ClickTarget::Action(Action::CloseOverlay),
    });
    let mut inner = shell.inner;
    hits.push(ClickRegion {
        area: shell.back,
        target: ClickTarget::Action(Action::OverlayBack),
    });

    match panel {
        Overlay::Objects(objects) => {
            if let Some(error) = &state.status
                && inner.height > stride
            {
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
                        Paragraph::new(single_line_external(label)).style(menu_style(
                            index == objects.selected && state.keyboard.is_overlay(),
                        )),
                        row,
                    );
                    hits.push(ClickRegion {
                        area: row,
                        target: ClickTarget::Action(Action::SelectObject(index)),
                    });
                }
            }
        }
        Overlay::Models(models) => match &models.screen {
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
                    let stride = stride.min(inner.height.max(1));
                    let capacity = usize::from(inner.height / stride);
                    let selected = (*selected).min(models.row_count().saturating_sub(1));
                    let start = selected.saturating_sub(capacity.saturating_sub(1));
                    for index in (start..models.row_count()).take(capacity) {
                        let row = Rect::new(
                            inner.x,
                            inner.y + (index - start) as u16 * stride,
                            inner.width,
                            stride,
                        );
                        if let Some(choice) = models.choices.get(index) {
                            render_model_choice(
                                frame,
                                row,
                                state,
                                choice,
                                index == selected && state.keyboard.is_overlay(),
                                stride > 1,
                            );
                        } else {
                            let label = if index == models.choices.len() {
                                "+ Add account or API…"
                            } else {
                                "Manage connections…"
                            };
                            frame.render_widget(
                                Paragraph::new(label).style(model_menu_style(
                                    index == selected && state.keyboard.is_overlay(),
                                )),
                                row,
                            );
                        }
                        hits.push(ClickRegion {
                            area: row,
                            target: ClickTarget::Action(Action::SelectModel(index)),
                        });
                    }
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
                            Paragraph::new(super::sanitize_external(&label)).style(menu_style(
                                index == *selected && state.keyboard.is_overlay(),
                            )),
                            row,
                        );
                        hits.push(ClickRegion {
                            area: row,
                            target: ClickTarget::Action(Action::SelectModel(index)),
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
                let text = super::sanitize_external(&text);
                let starts = crate::ui::selection::source_starts(&text, usize::from(inner.width));
                let retry_index = starts.len();
                let max =
                    (retry_index + usize::from(retry)).saturating_sub(usize::from(inner.height));
                let start = state.overlay_scroll.min(max);
                hits.push_scroll(
                    area,
                    crate::ui::interaction::ScrollTarget::OverlayContent { max },
                );
                render_copyable_text(frame, hits, inner, text, &starts, start);
                if retry && retry_index >= start && retry_index < start + usize::from(inner.height)
                {
                    let action = Rect::new(
                        inner.x,
                        inner.y + (retry_index - start) as u16,
                        inner.width,
                        1,
                    );
                    frame.render_widget(
                        Paragraph::new("Retry sign-in")
                            .style(menu_style(state.keyboard.is_overlay())),
                        action,
                    );
                    hits.push(ClickRegion {
                        area: action,
                        target: ClickTarget::Action(Action::RetryLogin),
                    });
                }
            }
            ModelScreen::Add { .. } | ModelScreen::Advanced { .. } | ModelScreen::Setup(_) => {}
        },
        Overlay::Help => {
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
                "Mouse drag    Select; release to copy".to_owned(),
                "/model        Models & connections".to_owned(),
                "/reload-config Reload configuration files".to_owned(),
                "/details      Latest task / tool".to_owned(),
            ];
            let text = lines.join("\n");
            let starts = crate::ui::selection::source_starts(&text, usize::from(inner.width));
            let max = starts.len().saturating_sub(usize::from(inner.height));
            hits.push_scroll(
                area,
                crate::ui::interaction::ScrollTarget::OverlayContent { max },
            );
            render_copyable_text(
                frame,
                hits,
                inner,
                text,
                &starts,
                state.overlay_scroll.min(max),
            );
        }
    }
    Some(area)
}
fn render_copyable_text(
    frame: &mut Frame<'_>,
    hits: &mut SurfaceHits,
    area: Rect,
    text: String,
    starts: &[u32],
    start: usize,
) {
    use crate::ui::selection::{CopySource, SelectableText, TextRow, display_text, row_range};
    let mut lines = Vec::new();
    let mut rows = Vec::new();
    for source_row in (start..starts.len()).take(usize::from(area.height)) {
        let range = row_range(&text, starts, source_row);
        lines.push(Line::from(display_text(&text[range.clone()])));
        rows.push(TextRow::new(
            Rect::new(area.x, area.y + rows.len() as u16, area.width, 1),
            &text,
            range,
        ));
    }
    frame.render_widget(Paragraph::new(lines).style(theme::body(INK)), area);
    hits.push_text(SelectableText {
        source: CopySource::Overlay,
        item: 0,
        text: text.into(),
        rows,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{ModelScreen, ObjectPanel};
    use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, style::Modifier};

    fn models(screen: ModelScreen) -> Overlay {
        let mut models = ModelPanel::new(None);
        models.screen = screen;
        Overlay::Models(models)
    }

    fn objects(choices: Vec<(crate::state::reader::ReaderSource, String)>) -> Overlay {
        Overlay::Objects(ObjectPanel {
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
                Overlay::Help,
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
                state.overlay = Some(panel);
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
                }
                let back = plan
                    .hit_regions()
                    .into_iter()
                    .find(|region| region.target == ClickTarget::Action(Action::OverlayBack))
                    .unwrap();
                assert_eq!(
                    plan.hit(back.area.x, back.area.y),
                    Some(ClickTarget::Action(Action::OverlayBack))
                );
            }
        }
    }

    #[test]
    fn panel_title_uses_a_neutral_active_surface_without_orange() {
        let mut state = UiState::default();
        state.overlay = Some(Overlay::Help);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let mut overlay = None;
        terminal
            .draw(|frame| {
                overlay = crate::view::render(frame, &state).overlay_area();
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
        let overlay = overlay.unwrap();
        for y in overlay.y..overlay.bottom() {
            for x in overlay.x..overlay.right() {
                let cell = &buffer[(x, y)];
                assert_ne!(cell.fg, theme::FOCUS_MARK);
                assert_ne!(cell.bg, theme::FOCUS_MARK);
            }
        }
    }

    #[test]
    fn help_names_only_the_declared_focus_and_editor_shortcuts() {
        let mut state = UiState::default();
        state.overlay = Some(Overlay::Help);
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
            state.overlay = Some(Overlay::Models(models));
            state.enter_overlay();
            for selected in 0..row_count {
                let Some(Overlay::Models(models)) = &mut state.overlay else {
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
                    .into_iter()
                    .find(|hit| hit.target == ClickTarget::Action(Action::SelectModel(selected)))
                    .expect("selected action remains visible");
                assert_eq!(
                    plan.hit(hit.area.x, hit.area.y),
                    Some(ClickTarget::Action(Action::SelectModel(selected)))
                );
                let buffer = terminal.backend().buffer();
                assert_eq!(buffer[(hit.area.x, hit.area.y)].bg, theme::PANEL);
                assert_ne!(buffer[(hit.area.x, hit.area.y)].bg, theme::FOCUS_MARK);
                for y in 0..height {
                    let line: String = (0..width).map(|x| buffer[(x, y)].symbol()).collect();
                    if line.trim() == "Connections" {
                        assert!(!plan.hit_regions().into_iter().any(|hit| hit.area.y == y
                            && matches!(hit.target, ClickTarget::Action(Action::SelectModel(_)))));
                    }
                }
            }
            let Some(Overlay::Models(models)) = &mut state.overlay else {
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
                            .into_iter()
                            .any(|hit| hit.target == ClickTarget::Action(Action::SelectModel(0)))
                    );
                })
                .unwrap();
        }
    }

    #[test]
    fn compact_error_state_keeps_each_selected_model_action_reachable() {
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
        state.overlay = Some(Overlay::Models(models));
        let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
        for index in [1, 2, 3] {
            let Some(Overlay::Models(models)) = state.overlay.as_mut() else {
                unreachable!()
            };
            models.screen = ModelScreen::List { selected: index };
            let mut plan = None;
            terminal
                .draw(|frame| plan = Some(crate::view::render(frame, &state)))
                .unwrap();
            let plan = plan.unwrap();
            let hit = plan
                .hit_regions()
                .into_iter()
                .find(|hit| hit.target == ClickTarget::Action(Action::SelectModel(index)))
                .expect("selected action stays visible in the bounded viewport");
            assert_eq!(plan.hit(hit.area.x, hit.area.y), Some(hit.target.clone()));
        }
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
        state.overlay = Some(Overlay::Models(models));
        let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
        let mut plan = None;
        terminal
            .draw(|frame| plan = Some(crate::view::render(frame, &state)))
            .unwrap();

        assert!(
            plan.unwrap()
                .hit_regions()
                .into_iter()
                .any(|hit| { hit.target == ClickTarget::Action(Action::RetryLogin) })
        );
    }
}
