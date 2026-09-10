use super::*;

pub(super) fn render_global_bar(frame: &mut Frame<'_>, plan: &LayoutPlan, state: &UiState) {
    let workspace = state
        .workspace
        .as_ref()
        .map(|(_, label)| sanitize_external(label))
        .unwrap_or_else(|| "正在打开工作区".into());
    components::global_bar::render(
        frame,
        plan.global_bar,
        GlobalBarProps {
            active: state.main,
            focused: state.focus == Focus::Global,
            selection: state.global_selection,
            attention_count: state.attention.len(),
            workspace: &workspace,
            navigation: &plan.hit_regions,
            ink: INK,
            muted: MUTED,
            accent: ACCENT,
            background: RAIL,
        },
    );
}

pub(super) fn render_session_rail(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &UiState,
) -> Vec<HitRegion> {
    let items = state.sessions.iter().map(|session| {
        let selected = state.selected == Some(session.id);
        let unread = state.session_ui.get(&session.id).map_or(0, |ui| ui.unread);
        let marker = if selected { "▌" } else { " " };
        let suffix = if unread > 0 {
            format!("  {unread}")
        } else if session.archived {
            "  已归档".into()
        } else {
            String::new()
        };
        SessionRailItem {
            label: format!("{marker} {}{suffix}", single_line_external(&session.title)),
            selected,
        }
    });
    components::session_rail::render(
        frame,
        area,
        SessionRailProps {
            items: items.collect(),
            ink: INK,
            muted: MUTED,
            accent: ACCENT,
            background: RAIL,
            selected_background: Color::Rgb(38, 45, 53),
        },
    )
}

pub(super) fn render_main(frame: &mut Frame<'_>, area: Rect, state: &UiState) {
    match state.main {
        MainView::Workbench => render_workbench(frame, area, state),
        MainView::Sessions => render_session_browser(frame, area, state),
        MainView::Attention => render_attention(frame, area, state),
        MainView::Settings => render_settings(frame, area, state),
    }
}

pub(super) fn render_session_browser(frame: &mut Frame<'_>, area: Rect, state: &UiState) {
    let (list_area, rename_area, archive_area) = session_browser_areas(area);
    let items = state.sessions.iter().map(|session| {
        let selected = state.selected == Some(session.id);
        let status = if session.archived {
            "已归档"
        } else {
            "可打开"
        };
        ListItem::new(Line::from(vec![
            Span::styled(
                if selected { "▌ " } else { "  " },
                Style::default().fg(ACCENT),
            ),
            Span::styled(
                single_line_external(&session.title),
                if selected {
                    Style::default()
                        .fg(INK)
                        .bg(Color::Rgb(38, 45, 53))
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(INK)
                },
            ),
            Span::styled(format!("   {status}"), Style::default().fg(MUTED)),
        ]))
    });
    frame.render_widget(section_block("会话管理"), area);
    frame.render_widget(List::new(items), list_area);

    let Some(selected) = state
        .selected
        .and_then(|id| state.sessions.iter().find(|session| session.id == id))
    else {
        frame.render_widget(
            Paragraph::new("选择一行后可重命名、归档或恢复")
                .style(Style::default().fg(MUTED))
                .alignment(Alignment::Center),
            Rect::new(
                rename_area.x,
                rename_area.y,
                rename_area.width.saturating_add(archive_area.width),
                rename_area.height,
            ),
        );
        return;
    };
    let busy = state
        .session_management_operations
        .contains_key(&selected.id);
    render_action_button(
        frame,
        rename_area,
        ActionButton {
            label: "[ 重命名 ]",
            foreground: if busy { MUTED } else { ACCENT },
            background: PANEL,
            bold: false,
        },
    );
    render_action_button(
        frame,
        archive_area,
        ActionButton {
            label: if busy {
                "[ 正在保存… ]"
            } else if selected.archived {
                "[ 恢复会话 ]"
            } else {
                "[ 归档会话 ]"
            },
            foreground: if busy { MUTED } else { ATTENTION },
            background: PANEL,
            bold: false,
        },
    );
}

pub(super) fn session_browser_areas(area: Rect) -> (Rect, Rect, Rect) {
    let inner = Rect::new(
        area.x.saturating_add(2),
        area.y.saturating_add(2),
        area.width.saturating_sub(4),
        area.height.saturating_sub(4),
    );
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(2)])
        .split(inner);
    let controls = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(rows[1]);
    (rows[0], controls[0], controls[1])
}

pub(super) fn session_browser_regions(area: Rect, state: &UiState) -> Vec<HitRegion> {
    let (list_area, rename_area, archive_area) = session_browser_areas(area);
    let mut regions = Vec::new();
    for (index, _) in state.sessions.iter().enumerate() {
        let y = list_area.y.saturating_add(index as u16);
        if y >= list_area.bottom() {
            break;
        }
        regions.push(HitRegion {
            area: Rect::new(list_area.x, y, list_area.width, 1),
            target: HitTarget::Session(index),
        });
    }
    if let Some(selected) = state.selected
        && state.sessions.iter().any(|session| session.id == selected)
        && !state.session_management_operations.contains_key(&selected)
    {
        regions.push(HitRegion {
            area: rename_area,
            target: HitTarget::RenameSession,
        });
        regions.push(HitRegion {
            area: archive_area,
            target: HitTarget::ArchiveSession,
        });
    }
    regions
}

pub(super) fn render_action_bar(frame: &mut Frame<'_>, plan: &LayoutPlan, state: &UiState) {
    let status = state
        .status
        .as_deref()
        .map(sanitize_external)
        .unwrap_or_default();
    let hint = if state.main == MainView::Workbench {
        " Tab 切换区域 · Shift+Enter 换行"
    } else {
        " Esc 返回工作台"
    };
    components::action_bar::render(
        frame,
        plan.action_bar,
        ActionBarProps {
            message: if status.is_empty() { hint } else { &status },
            is_status: !status.is_empty(),
            actions: &plan.hit_regions,
            selected_target: (state.focus == Focus::Actions).then(|| {
                if state.main != MainView::Workbench {
                    HitTarget::Quit
                } else {
                    [
                        HitTarget::Acceptance,
                        HitTarget::Submit,
                        HitTarget::Stop,
                        HitTarget::Quit,
                    ][state.action_selection.min(3)]
                }
            }),
            ink: INK,
            muted: MUTED,
            danger: DANGER,
            background: RAIL,
        },
    );
}

pub(super) fn render_too_small(frame: &mut Frame<'_>, plan: &LayoutPlan, state: &UiState) {
    let draft = state.selected_ui().is_some_and(|ui| !ui.draft.is_empty());
    frame.render_widget(
        Paragraph::new(format!(
            "窗口过小（{}×{}）\n至少需要 40×12。{}",
            plan.screen.width,
            plan.screen.height,
            if draft { " 草稿仍保留。" } else { "" }
        ))
        .alignment(Alignment::Center)
        .style(Style::default().fg(ATTENTION)),
        plan.main_surface,
    );
    render_action_bar(frame, plan, state);
}

pub(super) fn runtime_label(runtime: &RuntimeState) -> &'static str {
    match runtime {
        RuntimeState::Detached => "就绪",
        RuntimeState::Starting => "正在启动",
        RuntimeState::Running { .. } => "工作中",
        RuntimeState::Closing { .. } => "正在停止",
    }
}
