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
use unicode_width::UnicodeWidthStr;

const TITLE_INSET: u16 = 2;

/// The trailing tab that starts a new connection.
const ADD_TAB_LABEL: &str = "+ Add connection";

/// Tabs are separated by a vertical rule that owns no click target.
const TAB_SEPARATOR: &str = "│";
const TAB_SEPARATOR_WIDTH: usize = 1;

/// The tab screen names its own keys instead of the shell's generic hint.
const TAB_SHORTCUTS: &str =
    "esc close · ←/→ connection · ↑↓ model · enter choose · d delete · e edit";

pub(super) fn keyboard_hint(state: &UiState) -> &'static str {
    if state.keyboard.is_overlay() {
        if matches!(
            &state.overlay,
            Some(Overlay::Models(models)) if matches!(models.screen, ModelScreen::Tab { .. })
        ) {
            TAB_SHORTCUTS
        } else if matches!(
            &state.overlay,
            Some(Overlay::Models(models)) if matches!(models.screen, ModelScreen::Reasoning { .. })
        ) {
            "↑↓/←→ choose · enter apply · esc back"
        } else {
            "↑↓ move · enter choose · f6 input"
        }
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
            &models.screen,
            ModelScreen::Kind { .. }
                | ModelScreen::ModelInput { .. }
                | ModelScreen::Setup(_)
                | ModelScreen::ModelForm(_)
                | ModelScreen::ConfirmDelete { .. }
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
            ModelScreen::Tab { .. } => (
                (models.tab_row_count() * usize::from(stride)
                    + state.model_configuration_summary().lines().count()
                    + 5
                    + if state.status.is_some() { 2 } else { 0 })
                .clamp(7, if spacious { 24 } else { 16 }) as u16,
                "Connections & models",
            ),
            ModelScreen::Reasoning { .. } => (
                (crate::state::REASONING_EFFORTS.len() * usize::from(stride) + 4)
                    .clamp(5, if spacious { 22 } else { 14 }) as u16,
                "Choose reasoning",
            ),
            ModelScreen::Login { .. } => (10, "Sign in to ChatGPT"),
            ModelScreen::Kind { .. }
            | ModelScreen::ModelInput { .. }
            | ModelScreen::Setup(_)
            | ModelScreen::ModelForm(_)
            | ModelScreen::ConfirmDelete { .. } => {
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
                ModelOperationKind::Apply => "Applying model…",
                ModelOperationKind::Delete => "Deleting connection…",
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
            ModelScreen::Tab { selected } => {
                // The tab screen names its keys on the escape row. In a compact
                // layout that row is the last row of the body, so keep the body
                // above it instead of painting over the hint.
                if shell.back.y >= inner.y && shell.back.y < inner.bottom() {
                    inner.height = shell.back.y.saturating_sub(inner.y);
                }
                if inner.height == 0 {
                    return Some(area);
                }
                render_tab_strip(
                    frame,
                    hits,
                    models,
                    Rect::new(inner.x, inner.y, inner.width, 1),
                );
                inner.y = inner.y.saturating_add(1);
                inner.height = inner.height.saturating_sub(1);
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
                    // A connection delete keeps the strip on screen and reports
                    // what it is doing, so the panel never looks idle mid-delete.
                    let label = match operation.kind {
                        ModelOperationKind::Load => "Loading models…",
                        ModelOperationKind::Apply => "Applying model…",
                        ModelOperationKind::Delete => "Deleting connection…",
                    };
                    frame.render_widget(
                        Paragraph::new(label).style(Style::default().fg(MUTED)),
                        inner,
                    );
                } else {
                    let stride = stride.min(inner.height.max(1));
                    let capacity = usize::from(inner.height / stride);
                    if capacity == 0 {
                        return Some(area);
                    }
                    let rows = models.tab_models();
                    let total = models.tab_row_count();
                    let selected = (*selected).min(total.saturating_sub(1));
                    let start = selected.saturating_sub(capacity.saturating_sub(1));
                    for index in start..total.min(start + capacity) {
                        let row = Rect::new(
                            inner.x,
                            inner.y + (index - start) as u16 * stride,
                            inner.width,
                            stride,
                        );
                        let selected = index == selected && state.keyboard.is_overlay();
                        if let Some(selection) = rows.get(index) {
                            render_model_selection(
                                frame,
                                row,
                                state,
                                selection,
                                models.tab_profile(),
                                selected,
                            );
                        } else {
                            // The last row of every tab types a model ID the
                            // catalog does not publish; on the add tab it opens
                            // the connection-kind picker instead.
                            render_manual_model_row(frame, row, models.tab_is_add(), selected);
                        }
                        hits.push(ClickRegion {
                            area: row,
                            target: ClickTarget::Action(Action::SelectModel(index)),
                        });
                    }
                }
            }
            ModelScreen::Reasoning { selected, .. } => {
                let capacity = usize::from(inner.height / stride).max(1);
                let selected = (*selected).min(crate::state::REASONING_EFFORTS.len() - 1);
                let start = selected.saturating_sub(capacity.saturating_sub(1));
                for (index, effort) in crate::state::REASONING_EFFORTS
                    .iter()
                    .enumerate()
                    .skip(start)
                    .take(capacity)
                {
                    let row = Rect::new(
                        inner.x,
                        inner.y + (index - start) as u16 * stride,
                        inner.width,
                        stride,
                    );
                    let label = reasoning_label(*effort);
                    frame.render_widget(
                        Paragraph::new(single_line_external(label))
                            .style(menu_style(index == selected && state.keyboard.is_overlay())),
                        row,
                    );
                    hits.push(ClickRegion {
                        area: row,
                        target: ClickTarget::Action(Action::SelectReasoning(index)),
                    });
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
            ModelScreen::Kind { .. }
            | ModelScreen::ModelInput { .. }
            | ModelScreen::Setup(_)
            | ModelScreen::ModelForm(_)
            | ModelScreen::ConfirmDelete { .. } => {}
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
                models(ModelScreen::Tab { selected: 0 }),
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

fn model_row_label(state: &UiState, selection: &bone_app::ModelSelection) -> String {
    let mut parts = Vec::new();
    if let Some(marker) = state.model_selection_marker(selection) {
        parts.push(format!("✓ {marker}"));
    }
    parts.push(selection.model.clone());
    if let Some(effort) = crate::state::model_effort(selection) {
        parts.push(effort.as_str().into());
    }
    parts.join(" · ")
}

fn reasoning_label(effort: bone_app::ReasoningEffort) -> &'static str {
    match effort {
        bone_app::ReasoningEffort::None => "provider default",
        bone_app::ReasoningEffort::Minimal => "minimal",
        bone_app::ReasoningEffort::Low => "low",
        bone_app::ReasoningEffort::Medium => "medium",
        bone_app::ReasoningEffort::High => "high",
        bone_app::ReasoningEffort::Xhigh => "xhigh",
        bone_app::ReasoningEffort::Max => "max",
    }
}

/// One model row of a connection tab. The tab already names the connection, so
/// the row only has to identify the model and its state.
fn render_model_selection(
    frame: &mut Frame<'_>,
    row: Rect,
    state: &UiState,
    selection: &bone_app::ModelSelection,
    profile: Option<&bone_app::Profile>,
    selected: bool,
) {
    let background = if selected { theme::PANEL } else { INPUT };
    let pointer = if selected { "› " } else { "  " };
    let pointer_tone = if selected { INFO } else { background };
    let title = model_row_label(state, selection);
    let mut lines = vec![Line::from(vec![
        Span::styled(pointer, theme::label_on(pointer_tone, background)),
        Span::styled(
            super::single_line_external(&title),
            theme::label_on(INK, background),
        ),
    ])];
    if row.height > 1
        && let Some(profile) = profile
    {
        lines.push(Line::from(vec![
            Span::styled("  ", theme::body_on(MUTED, background)),
            Span::styled(
                super::single_line_external(&profile.label),
                theme::body_on(MUTED, background),
            ),
        ]));
    }
    frame.render_widget(Paragraph::new(lines).style(theme::surface(background)), row);
}

/// The last row of a tab: the manual model editor, or the entry that opens the
/// connection-kind picker when the trailing add tab is selected.
fn render_manual_model_row(frame: &mut Frame<'_>, row: Rect, add_tab: bool, selected: bool) {
    let background = if selected { theme::PANEL } else { INPUT };
    let label = if add_tab {
        "+ Add connection…"
    } else {
        "Enter a model ID…"
    };
    let pointer = if selected { "› " } else { "  " };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                pointer,
                theme::label_on(if selected { INFO } else { background }, background),
            ),
            Span::styled(label, theme::label_on(INK, background)),
        ]))
        .style(theme::surface(background)),
        row,
    );
}

/// Draw the connection tabs and register one click target per tab.
///
/// The strip is windowed by display width so the current tab always stays
/// visible; `‹` and `›` mark the ends the window hides.
fn render_tab_strip(
    frame: &mut Frame<'_>,
    hits: &mut SurfaceHits,
    models: &crate::state::ModelPanel,
    strip: Rect,
) {
    if strip.width == 0 || strip.height == 0 {
        return;
    }
    let mut labels: Vec<String> = models
        .profiles
        .iter()
        .map(|profile| format!(" {} ", single_line_external(&profile.label)))
        .collect();
    labels.push(format!(" {ADD_TAB_LABEL} "));
    let widths: Vec<usize> = labels
        .iter()
        .map(|label| UnicodeWidthStr::width(label.as_str()))
        .collect();
    let tab = models.tab.min(labels.len() - 1);
    let (start, end, left_hidden, right_hidden) =
        tab_window(&widths, tab, usize::from(strip.width), TAB_SEPARATOR_WIDTH);
    let mut x = strip.x;
    if left_hidden {
        frame.render_widget(
            Paragraph::new("‹").style(Style::default().fg(MUTED)),
            Rect::new(x, strip.y, 1, 1),
        );
        x += 1;
    }
    let right_edge = strip.right().saturating_sub(u16::from(right_hidden));
    for index in start..end {
        let remaining = right_edge.saturating_sub(x);
        if remaining == 0 {
            break;
        }
        // A profile label may outgrow the whole strip. Clip it rather than
        // dropping it: the tab the user is on must stay painted and clickable.
        let requested = widths[index] as u16;
        let width = requested.min(remaining);
        let row = Rect::new(x, strip.y, width, 1);
        let style = if index == tab {
            theme::label_on(INFO, theme::PANEL)
        } else {
            theme::body_on(MUTED, INPUT)
        };
        frame.render_widget(Paragraph::new(labels[index].as_str()).style(style), row);
        hits.push(ClickRegion {
            area: row,
            target: ClickTarget::Action(Action::SelectTab(index)),
        });
        x += width;
        if width < requested {
            // Nothing after an over-wide tab can fit on this strip.
            break;
        }
        if index + 1 < end {
            frame.render_widget(
                Paragraph::new(TAB_SEPARATOR).style(Style::default().fg(theme::STRUCTURE)),
                Rect::new(x, strip.y, TAB_SEPARATOR_WIDTH as u16, 1),
            );
            x += TAB_SEPARATOR_WIDTH as u16;
        }
    }
    if right_hidden {
        frame.render_widget(
            Paragraph::new("›").style(Style::default().fg(MUTED)),
            Rect::new(strip.right() - 1, strip.y, 1, 1),
        );
    }
}

/// The contiguous tab window that keeps `tab` visible.
///
/// The window never splits a tab, so the caller only has to report whether
/// either end was cut off. A tab wider than the strip still gets the window to
/// itself and is clipped by the caller.
fn tab_window(
    widths: &[usize],
    tab: usize,
    available: usize,
    separator: usize,
) -> (usize, usize, bool, bool) {
    let mut start = tab;
    let mut end = tab + 1;
    let mut used = widths[tab];
    loop {
        let mut grew = false;
        if end < widths.len() {
            let cost = separator + widths[end];
            if used + cost <= available {
                used += cost;
                end += 1;
                grew = true;
            }
        }
        if start > 0 {
            let cost = separator + widths[start - 1];
            if used + cost <= available {
                used += cost;
                start -= 1;
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    // The truncation markers take one column each; drop tabs away from the
    // cursor until both the window and its markers fit.
    loop {
        let markers = usize::from(start > 0) + usize::from(end < widths.len());
        if used + markers <= available {
            break;
        }
        let right = end - tab;
        let left = tab - start;
        if end < widths.len() && (right >= left || start == tab) && end > start + 1 {
            end -= 1;
            used -= separator + widths[end];
        } else if start < tab && start + 1 < end {
            used -= separator + widths[start];
            start += 1;
        } else if end > start + 1 {
            end -= 1;
            used -= separator + widths[end];
        } else {
            break;
        }
    }
    (start, end, start > 0, end < widths.len())
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
mod tab_strip_tests {
    use super::*;
    use crate::state::{ModelChoice, ModelPanel, ModelScreen, Overlay};
    use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

    fn profile(id: &str) -> bone_app::Profile {
        bone_app::Profile::new(
            bone_app::ProfileId::new(id).unwrap(),
            id,
            bone_app::EndpointConfig::OpenAiChatCompletions {
                base_url: Some(format!("https://{id}.example.test/v1")),
            },
        )
        .unwrap()
    }

    fn choice(profile: &bone_app::Profile, model: &str) -> ModelChoice {
        ModelChoice {
            selection: bone_app::ModelSelection::new(profile.id.clone(), model).unwrap(),
            profile_label: profile.label.clone(),
            label: model.into(),
        }
    }

    /// A tab screen with `connections` saved connections and `models` catalog
    /// entries on each of them.
    fn tab_state(connections: usize, models: usize) -> UiState {
        let mut panel = ModelPanel::new(None);
        for index in 0..connections {
            let profile = profile(&format!("connection-{index}"));
            for model in 0..models {
                panel
                    .choices
                    .push(choice(&profile, &format!("model-{index}-{model}")));
            }
            panel.profiles.push(profile);
        }
        panel.screen = ModelScreen::Tab { selected: 0 };
        let mut state = UiState::default();
        state.overlay = Some(Overlay::Models(panel));
        state
    }

    fn buffer_text(buffer: &Buffer) -> String {
        buffer
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>()
    }

    fn row_text(buffer: &Buffer, y: u16) -> String {
        (buffer.area.x..buffer.area.right())
            .map(|x| buffer[(x, y)].symbol())
            .collect()
    }

    fn draw(state: &UiState, width: u16, height: u16) -> (crate::view::FrameSnapshot, Buffer) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let mut plan = None;
        terminal
            .draw(|frame| plan = Some(crate::view::render(frame, state)))
            .unwrap();
        (plan.unwrap(), terminal.backend().buffer().clone())
    }

    fn tab_hits(plan: &crate::view::FrameSnapshot) -> Vec<usize> {
        plan.hit_regions()
            .into_iter()
            .filter_map(|hit| match hit.target {
                ClickTarget::Action(Action::SelectTab(index)) => Some(index),
                _ => None,
            })
            .collect()
    }

    fn select_tab(state: &mut UiState, tab: usize) {
        let Some(Overlay::Models(panel)) = &mut state.overlay else {
            unreachable!()
        };
        panel.tab = tab;
    }

    #[test]
    fn every_connection_and_the_add_tab_own_a_click_target() {
        // A wide strip paints every tab.
        let state = tab_state(3, 2);
        let (plan, _) = draw(&state, 160, 40);
        for index in 0..4 {
            let region = plan
                .hit_regions()
                .into_iter()
                .find(|hit| hit.target == ClickTarget::Action(Action::SelectTab(index)))
                .unwrap_or_else(|| panic!("tab {index} is missing on a wide strip"));
            assert_eq!(
                plan.hit(region.area.x, region.area.y),
                Some(ClickTarget::Action(Action::SelectTab(index)))
            );
        }

        // A narrow strip windows the tabs, and every tab it paints is clickable.
        for (width, height) in [(40, 12), (60, 16), (80, 24)] {
            let state = tab_state(3, 2);
            let (plan, _) = draw(&state, width, height);
            let hits = tab_hits(&plan);
            assert!(!hits.is_empty(), "no tab is clickable in {width}x{height}");
            for index in hits {
                let region = plan
                    .hit_regions()
                    .into_iter()
                    .find(|hit| hit.target == ClickTarget::Action(Action::SelectTab(index)))
                    .unwrap();
                assert_eq!(
                    plan.hit(region.area.x, region.area.y),
                    Some(ClickTarget::Action(Action::SelectTab(index)))
                );
            }
        }
    }

    #[test]
    fn the_current_tab_is_highlighted_and_the_others_stay_muted() {
        let mut state = tab_state(2, 1);
        select_tab(&mut state, 1);
        state.enter_overlay();
        let (plan, buffer) = draw(&state, 80, 24);

        let current = plan
            .hit_regions()
            .into_iter()
            .find(|hit| hit.target == ClickTarget::Action(Action::SelectTab(1)))
            .unwrap();
        let other = plan
            .hit_regions()
            .into_iter()
            .find(|hit| hit.target == ClickTarget::Action(Action::SelectTab(0)))
            .unwrap();
        assert_eq!(buffer[(current.area.x, current.area.y)].bg, theme::PANEL);
        assert_ne!(
            buffer[(current.area.x, current.area.y)].bg,
            theme::FOCUS_MARK
        );
        assert_eq!(buffer[(other.area.x, other.area.y)].bg, INPUT);
    }

    #[test]
    fn a_windowed_strip_keeps_the_current_tab_visible() {
        let mut state = tab_state(8, 1);
        select_tab(&mut state, 7);
        let (plan, buffer) = draw(&state, 40, 12);

        let current = plan
            .hit_regions()
            .into_iter()
            .find(|hit| hit.target == ClickTarget::Action(Action::SelectTab(7)))
            .expect("the current tab must stay painted");
        assert_eq!(
            plan.hit(current.area.x, current.area.y),
            Some(ClickTarget::Action(Action::SelectTab(7)))
        );
        // The strip reports the tabs it hid to the left.
        let strip = row_text(&buffer, current.area.y);
        assert!(strip.contains('‹'), "{strip:?}");
        assert!(!tab_hits(&plan).contains(&0));

        // A tab in the middle hides tabs at both ends.
        select_tab(&mut state, 3);
        let (plan, buffer) = draw(&state, 40, 12);
        let strip = row_text(
            &buffer,
            plan.hit_regions()
                .into_iter()
                .find(|hit| hit.target == ClickTarget::Action(Action::SelectTab(3)))
                .expect("the current tab must stay painted")
                .area
                .y,
        );
        assert!(strip.contains('‹'), "{strip:?}");
        assert!(strip.contains('›'), "{strip:?}");
        assert!(!tab_hits(&plan).contains(&8));
    }

    #[test]
    fn a_label_wider_than_the_strip_is_clipped_and_still_clickable() {
        let mut state = tab_state(0, 0);
        let Some(Overlay::Models(panel)) = &mut state.overlay else {
            unreachable!()
        };
        let label = "c".repeat(128);
        let mut wide = profile("wide");
        wide.label = label.clone();
        panel.profiles.push(wide);
        panel.screen = ModelScreen::Tab { selected: 0 };
        state.enter_overlay();

        let (plan, buffer) = draw(&state, 40, 12);
        let region = plan
            .hit_regions()
            .into_iter()
            .find(|hit| hit.target == ClickTarget::Action(Action::SelectTab(0)))
            .expect("an over-wide tab must not be dropped");
        assert!(region.area.width > 0);
        assert_eq!(
            plan.hit(region.area.x, region.area.y),
            Some(ClickTarget::Action(Action::SelectTab(0)))
        );
        assert!(row_text(&buffer, region.area.y).contains('c'));
    }

    #[test]
    fn the_last_row_of_a_connection_types_a_model_and_the_add_tab_adds_one() {
        let mut state = tab_state(1, 2);
        let (_, buffer) = draw(&state, 80, 24);
        assert!(buffer_text(&buffer).contains("Enter a model ID…"));

        select_tab(&mut state, 1);
        let (plan, buffer) = draw(&state, 80, 24);
        assert!(buffer_text(&buffer).contains("+ Add connection…"));
        assert!(!buffer_text(&buffer).contains("Enter a model ID…"));
        // The add tab holds only that one row.
        let rows = plan
            .hit_regions()
            .into_iter()
            .filter(|hit| matches!(hit.target, ClickTarget::Action(Action::SelectModel(_))))
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].target, ClickTarget::Action(Action::SelectModel(0)));
    }

    #[test]
    fn every_painted_model_row_is_clickable_inside_a_small_panel() {
        let mut state = tab_state(1, 6);
        state.status = Some("Model switch failed".into());
        state.enter_overlay();
        let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();

        // Walk the cursor down the tab so every row is painted at some point.
        for selected in 0..7 {
            let Some(Overlay::Models(panel)) = state.overlay.as_mut() else {
                unreachable!()
            };
            panel.screen = ModelScreen::Tab { selected };
            let mut plan = None;
            terminal
                .draw(|frame| plan = Some(crate::view::render(frame, &state)))
                .unwrap();
            let plan = plan.unwrap();
            let hit = plan
                .hit_regions()
                .into_iter()
                .find(|hit| hit.target == ClickTarget::Action(Action::SelectModel(selected)))
                .expect("the selected row stays inside the bounded viewport");
            assert_eq!(plan.hit(hit.area.x, hit.area.y), Some(hit.target.clone()));
        }
    }

    #[test]
    fn a_pending_delete_names_itself_and_keeps_the_strip() {
        let mut state = tab_state(2, 2);
        state.model_operation = Some(crate::state::ModelOperation {
            session: None,
            request: 7,
            kind: ModelOperationKind::Delete,
        });
        let (plan, buffer) = draw(&state, 80, 24);

        assert!(buffer_text(&buffer).contains("Deleting connection…"));
        assert!(buffer_text(&buffer).contains("connection-0"));
        assert!(buffer_text(&buffer).contains("connection-1"));
        assert!(!tab_hits(&plan).is_empty());
    }

    #[test]
    fn the_tab_screen_replaces_the_generic_escape_hint() {
        let mut state = tab_state(1, 1);
        state.enter_overlay();
        let (_, buffer) = draw(&state, 120, 30);
        let rendered = buffer_text(&buffer);
        for key in ["esc", "←/→", "↑↓", "enter", "d delete", "e edit"] {
            assert!(rendered.contains(key), "missing {key} in the tab hint");
        }
    }

    #[test]
    fn failed_login_has_a_pointer_retry_action() {
        let mut state = UiState::default();
        let mut panel = ModelPanel::new(None);
        panel.screen = ModelScreen::Login {
            request: 1,
            state: bone_app::LoginState::Failed {
                message: "authorization expired".into(),
            },
        };
        state.overlay = Some(Overlay::Models(panel));
        let (plan, _) = draw(&state, 40, 12);

        assert!(
            plan.hit_regions()
                .into_iter()
                .any(|hit| hit.target == ClickTarget::Action(Action::RetryLogin))
        );
    }
}
