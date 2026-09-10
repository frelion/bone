use super::*;
use std::rc::Rc;
struct WorkspaceChangeLayout {
    header: Rect,
    body: Rect,
    footer: Rect,
}

fn workspace_change_layout(panel: Rect) -> WorkspaceChangeLayout {
    let content = Rect::new(
        panel.x.saturating_add(2),
        panel.y.saturating_add(6),
        panel.width.saturating_sub(4),
        panel.height.saturating_sub(8),
    );
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(7.min(content.height)),
            Constraint::Min(0),
            Constraint::Length(2.min(content.height.saturating_sub(7))),
        ])
        .split(content);
    WorkspaceChangeLayout {
        header: rows[0],
        body: rows[1],
        footer: rows[2],
    }
}

pub(in crate::view) fn workspace_change_regions(panel: Rect, state: &UiState) -> Vec<HitRegion> {
    let layout = workspace_change_layout(panel);
    if layout.body.width == 0 {
        return Vec::new();
    }
    if state.workspace_changes.file.is_some() || state.workspace_changes.file_loading {
        let footer = workspace_file_footer(layout.footer);
        let mut regions = Vec::with_capacity(3);
        if !state.workspace_changes.file_loading && !state.workspace_changes.file_back.is_empty() {
            regions.push(HitRegion {
                area: footer[0],
                target: HitTarget::PreviousWorkspaceFile,
            });
        }
        if !state.workspace_changes.file_loading
            && state
                .workspace_changes
                .file
                .as_ref()
                .is_some_and(|page| page.next_cursor.is_some())
        {
            regions.push(HitRegion {
                area: footer[1],
                target: HitTarget::MoreWorkspaceFile,
            });
        }
        regions.push(HitRegion {
            area: footer[2],
            target: HitTarget::CloseWorkspaceFile,
        });
        return regions;
    }
    let (start, end) = workspace_change_window(state, usize::from(layout.body.height));
    let mut regions = (start..end)
        .enumerate()
        .map(|(row, index)| HitRegion {
            area: Rect::new(
                layout.body.x,
                layout.body.y.saturating_add(row as u16),
                layout.body.width,
                1,
            ),
            target: HitTarget::WorkspaceChange(index),
        })
        .collect::<Vec<_>>();
    let footer = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(34),
            Constraint::Percentage(33),
            Constraint::Percentage(33),
        ])
        .split(layout.footer);
    regions.push(HitRegion {
        area: footer[0],
        target: HitTarget::RefreshWorkspaceChanges,
    });
    if !state.workspace_changes.pages.back.is_empty() {
        regions.push(HitRegion {
            area: footer[1],
            target: HitTarget::NewerWorkspaceChanges,
        });
    }
    if state
        .workspace_changes
        .page
        .as_ref()
        .is_some_and(|page| page.next_cursor.is_some())
    {
        regions.push(HitRegion {
            area: footer[2],
            target: HitTarget::OlderWorkspaceChanges,
        });
    }
    regions
}

pub(super) fn render_workspace_changes(frame: &mut Frame<'_>, panel: Rect, state: &UiState) {
    let area = Rect::new(
        panel.x,
        panel.y.saturating_add(4),
        panel.width,
        panel.height.saturating_sub(4),
    );
    frame.render_widget(section_block("工作区变更"), area);
    let layout = workspace_change_layout(panel);
    let baseline = state.workspace_changes.page.as_ref().map_or_else(
        || "基线  正在读取…".into(),
        |page| baseline_label(&page.baseline),
    );
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled(baseline, Style::default().fg(ACCENT)),
            Line::styled(
                "这是整个工作区相对 Git HEAD 的差异，可能包含你或其他任务的修改。",
                Style::default().fg(ATTENTION),
            ),
            Line::styled(
                "任务证据会单独关联；这里不把文件变更猜成某个任务的产物。",
                Style::default().fg(MUTED),
            ),
        ])
        .wrap(Wrap { trim: false }),
        layout.header,
    );

    if let Some(file) = &state.workspace_changes.file {
        let end = file.offset.saturating_add(file.bytes_read);
        let range = file.total_bytes.map_or_else(
            || format!("字节 {}–{}", file.offset, end),
            |total| format!("字节 {}–{} / {total}", file.offset, end),
        );
        let loading = if state.workspace_changes.file_loading {
            " · 正在读取下一窗口…"
        } else {
            ""
        };
        let mut lines = vec![
            Line::styled(
                sanitize_external(&file.path),
                Style::default().fg(INK).add_modifier(Modifier::BOLD),
            ),
            Line::styled(
                format!("{}{loading}", workspace_file_source_label(file.source)),
                Style::default().fg(MUTED),
            ),
            Line::styled(range, Style::default().fg(MUTED)),
            Line::raw(""),
        ];
        match file.media {
            bone_app::WorkspaceFileMedia::Text => {
                lines.extend(components::paged_reader::indexed_visible_lines(
                    file.text.as_deref().unwrap_or(""),
                    &state.workspace_changes.file_layout,
                    state.detail_scroll,
                    layout.body.width,
                    layout.body.height.saturating_sub(4),
                ))
            }
            bone_app::WorkspaceFileMedia::Binary => {
                lines.push(Line::raw("这是二进制内容，App 未向终端返回原始字节。"))
            }
            bone_app::WorkspaceFileMedia::Missing => {
                lines.push(Line::raw("文件在当前工作树中已不存在。"));
            }
        }
        frame.render_widget(Paragraph::new(lines), layout.body);
        let footer = workspace_file_footer(layout.footer);
        render_workspace_file_control(frame, footer[0], "[ 上一窗口 ]", 0, state);
        render_workspace_file_control(frame, footer[1], "[ 继续读取 ]", 1, state);
        render_workspace_file_control(frame, footer[2], "[ 返回文件列表 ]", 2, state);
        return;
    }

    if state.workspace_changes.file_loading {
        frame.render_widget(
            Paragraph::new("正在由 App 读取所选文件…").style(Style::default().fg(MUTED)),
            layout.body,
        );
    } else {
        let items = match state.workspace_changes.page.as_ref() {
            Some(page) if !page.files.is_empty() => {
                let (start, end) = workspace_change_window(state, usize::from(layout.body.height));
                page.files[start..end]
                    .iter()
                    .enumerate()
                    .map(|(index, file)| {
                        let index = start + index;
                        let marker = if index == state.workspace_changes.selection {
                            "›"
                        } else {
                            " "
                        };
                        ListItem::new(format!(
                            "{marker} {:<5} {}",
                            workspace_file_state_label(file),
                            sanitize_external(&file.path)
                        ))
                        .style({
                            let keyboard_selected = state.focus == Focus::Detail
                                && state.focused_control == Some(HitTarget::WorkspaceChange(index));
                            if keyboard_selected {
                                Style::default().fg(RAIL).bg(ACCENT)
                            } else if index == state.workspace_changes.selection {
                                Style::default().fg(INK).bg(PANEL)
                            } else {
                                Style::default().fg(MUTED)
                            }
                        })
                    })
                    .collect::<Vec<_>>()
            }
            Some(page) if matches!(&page.baseline, bone_app::WorkspaceBaseline::NotGit) => {
                vec![ListItem::new("当前工作区不是 Git 仓库。").style(Style::default().fg(MUTED))]
            }
            Some(_) => {
                vec![ListItem::new("工作区相对 HEAD 没有变更。").style(Style::default().fg(MUTED))]
            }
            None => {
                vec![ListItem::new("正在由 App 读取变更列表…").style(Style::default().fg(MUTED))]
            }
        };
        frame.render_widget(List::new(items), layout.body);
    }
    for region in workspace_change_regions(panel, state) {
        let label = match region.target {
            HitTarget::RefreshWorkspaceChanges => "[ 刷新 ]",
            HitTarget::NewerWorkspaceChanges => "[ 较新文件 ]",
            HitTarget::OlderWorkspaceChanges => "[ 更多文件 ]",
            _ => continue,
        };
        let keyboard_selected =
            state.focus == Focus::Detail && state.focused_control == Some(region.target);
        frame.render_widget(
            Paragraph::new(label)
                .alignment(Alignment::Center)
                .style(if keyboard_selected {
                    Style::default().fg(RAIL).bg(ACCENT)
                } else {
                    Style::default().fg(ACCENT).bg(RAIL)
                }),
            region.area,
        );
    }
}

fn workspace_file_footer(area: Rect) -> Rc<[Rect]> {
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(30),
            Constraint::Percentage(30),
            Constraint::Percentage(40),
        ])
        .split(area)
}

fn render_workspace_file_control(
    frame: &mut Frame<'_>,
    area: Rect,
    label: &str,
    control: usize,
    state: &UiState,
) {
    let available = match control {
        0 => !state.workspace_changes.file_back.is_empty(),
        1 => state
            .workspace_changes
            .file
            .as_ref()
            .is_some_and(|page| page.next_cursor.is_some()),
        2 => true,
        _ => false,
    } && !state.workspace_changes.file_loading;
    let target = match control {
        0 => HitTarget::PreviousWorkspaceFile,
        1 => HitTarget::MoreWorkspaceFile,
        _ => HitTarget::CloseWorkspaceFile,
    };
    let selected = state.focus == Focus::Detail
        && (state.focused_control == Some(target)
            || (state.focused_control.is_none()
                && state.workspace_changes.file_control_selection == control));
    let style = if !available {
        Style::default().fg(MUTED).bg(RAIL)
    } else if selected {
        Style::default().fg(RAIL).bg(ACCENT)
    } else {
        Style::default().fg(ACCENT).bg(RAIL)
    };
    frame.render_widget(
        Paragraph::new(label)
            .alignment(Alignment::Center)
            .style(style),
        area,
    );
}

fn workspace_change_window(state: &UiState, height: usize) -> (usize, usize) {
    let length = state
        .workspace_changes
        .page
        .as_ref()
        .map_or(0, |page| page.files.len());
    if length == 0 || height == 0 {
        return (0, 0);
    }
    let selected = state.workspace_changes.selection.min(length - 1);
    let start = selected.saturating_sub(height - 1);
    (start, (start + height).min(length))
}

fn baseline_label(baseline: &bone_app::WorkspaceBaseline) -> String {
    match baseline {
        bone_app::WorkspaceBaseline::NotGit => "基线  非 Git 工作区".into(),
        bone_app::WorkspaceBaseline::Git { head: None } => "基线  Git（尚无 HEAD）".into(),
        bone_app::WorkspaceBaseline::Git { head: Some(head) } => {
            format!(
                "基线  Git HEAD {}",
                sanitize_external(&head.chars().take(12).collect::<String>())
            )
        }
    }
}

fn workspace_file_state_label(file: &bone_app::WorkspaceChangedFile) -> &'static str {
    use bone_app::GitFileState::*;
    let state = if file.worktree != Unchanged {
        file.worktree
    } else {
        file.index
    };
    match state {
        Unchanged => "未变",
        Modified => "修改",
        Added => "新增",
        Deleted => "删除",
        Renamed => "改名",
        Copied => "复制",
        TypeChanged => "类型",
        Unmerged => "冲突",
        Untracked => "未跟踪",
        Unknown => "未知",
    }
}

fn workspace_file_source_label(source: bone_app::WorkspaceFileSource) -> &'static str {
    match source {
        bone_app::WorkspaceFileSource::DiffAgainstHead => "App 提供的 HEAD 差异",
        bone_app::WorkspaceFileSource::WorkingTree => "App 提供的当前工作树正文",
    }
}
