use super::*;
struct ArtifactLayout {
    header: Rect,
    body: Rect,
    footer: Rect,
}

fn artifact_layout(panel: Rect) -> ArtifactLayout {
    let content = Rect::new(
        panel.x.saturating_add(2),
        panel.y.saturating_add(6),
        panel.width.saturating_sub(4),
        panel.height.saturating_sub(8),
    );
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(8.min(content.height)),
            Constraint::Min(0),
            Constraint::Length(2.min(content.height.saturating_sub(8))),
        ])
        .split(content);
    ArtifactLayout {
        header: rows[0],
        body: rows[1],
        footer: rows[2],
    }
}

pub(in crate::view) fn artifact_regions(panel: Rect, state: &UiState) -> Vec<HitRegion> {
    let layout = artifact_layout(panel);
    if layout.body.width == 0 {
        return Vec::new();
    }
    let footer = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(34),
            Constraint::Percentage(33),
            Constraint::Percentage(33),
        ])
        .split(layout.footer);
    if state.artifact.source.is_some() || state.artifact.source_loading {
        let mut regions = vec![HitRegion {
            area: footer[0],
            target: HitTarget::CloseEvidenceSource,
        }];
        if state
            .artifact
            .source
            .as_ref()
            .is_some_and(|source| source.window_offset > 0)
        {
            regions.push(HitRegion {
                area: footer[1],
                target: HitTarget::PreviousEvidenceSource,
            });
        }
        if state
            .artifact
            .source
            .as_ref()
            .is_some_and(|source| source.next_offset.is_some())
        {
            regions.push(HitRegion {
                area: footer[2],
                target: HitTarget::MoreEvidenceSource,
            });
        }
        return regions;
    }
    let (start, end) = evidence_window(state, usize::from(layout.body.height));
    let mut regions = (start..end)
        .enumerate()
        .map(|(row, index)| HitRegion {
            area: Rect::new(
                layout.body.x,
                layout.body.y.saturating_add(row as u16),
                layout.body.width,
                1,
            ),
            target: HitTarget::Evidence(index),
        })
        .collect::<Vec<_>>();
    regions.push(HitRegion {
        area: footer[0],
        target: HitTarget::RefreshArtifact,
    });
    if !state.artifact.evidence_pages.back.is_empty() {
        regions.push(HitRegion {
            area: footer[1],
            target: HitTarget::NewerEvidence,
        });
    }
    if state
        .artifact
        .evidence
        .as_ref()
        .is_some_and(|page| page.next_cursor.is_some())
    {
        regions.push(HitRegion {
            area: footer[2],
            target: HitTarget::OlderEvidence,
        });
    }
    regions
}

pub(super) fn render_artifact(frame: &mut Frame<'_>, panel: Rect, state: &UiState) {
    let area = Rect::new(
        panel.x,
        panel.y.saturating_add(4),
        panel.width,
        panel.height.saturating_sub(4),
    );
    frame.render_widget(section_block("产物与证据"), area);
    let layout = artifact_layout(panel);
    let header = match &state.artifact.artifact {
        Some(artifact) => vec![
            Line::styled(
                format!("结果版本 {}", artifact.result.version.0),
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Line::styled(
                single_line_external(&artifact.summary),
                Style::default().fg(INK),
            ),
            Line::styled(
                format!("明确引用的来源：{}", artifact.evidence_count),
                Style::default().fg(MUTED),
            ),
            Line::styled(
                "来源只来自结果的显式引用；不会从自然语言、文件名或 diff 猜测。",
                Style::default().fg(ATTENTION),
            ),
        ],
        None if state.artifact.loading => vec![Line::styled(
            "正在由 App 读取最新持久结果及其明确来源…",
            Style::default().fg(MUTED),
        )],
        None => vec![Line::styled(
            "当前还没有可展示的持久产物。",
            Style::default().fg(MUTED),
        )],
    };
    frame.render_widget(
        Paragraph::new(header).wrap(Wrap { trim: false }),
        layout.header,
    );

    if let Some(source) = &state.artifact.source {
        let (title, explanation) = match &source.availability {
            bone_app::EvidenceAvailability::Available { kind, title } => (
                sanitize_external(title),
                evidence_kind_label(*kind).to_owned(),
            ),
            bone_app::EvidenceAvailability::Private => (
                "私有来源".into(),
                "App 明确将此记录标为私有；标题与正文均不会暴露。".into(),
            ),
            bone_app::EvidenceAvailability::Missing => (
                "来源暂不可用".into(),
                if source.projection_pending {
                    "旧数据的来源投影仍在有界回填，暂不宣称永久缺失。".into()
                } else {
                    "显式引用存在，但 App 无法定位来源正文。".into()
                },
            ),
        };
        let mut lines = vec![
            Line::styled(title, Style::default().fg(INK).add_modifier(Modifier::BOLD)),
            Line::styled(explanation, Style::default().fg(MUTED)),
            Line::raw(""),
        ];
        lines.extend(components::paged_reader::indexed_visible_lines(
            &source.text,
            &source.layout,
            state.detail_scroll,
            layout.body.width,
            layout.body.height.saturating_sub(4),
        ));
        if source.frontend_truncated {
            lines.push(Line::styled(
                "前端仅保留 1 MiB 阅读窗口；可用“上一段 / 继续读取”往返。",
                Style::default().fg(ATTENTION),
            ));
        }
        frame.render_widget(Paragraph::new(lines), layout.body);
    } else if state.artifact.source_loading {
        frame.render_widget(
            Paragraph::new("正在由 App 分页读取来源正文…").style(Style::default().fg(MUTED)),
            layout.body,
        );
    } else {
        let items = match state.artifact.evidence.as_ref() {
            Some(page) if !page.items.is_empty() => {
                let (start, end) = evidence_window(state, usize::from(layout.body.height));
                page.items[start..end]
                    .iter()
                    .enumerate()
                    .map(|(offset, evidence)| {
                        let index = start + offset;
                        let marker = if index == state.artifact.selection {
                            "›"
                        } else {
                            " "
                        };
                        let label = match &evidence.availability {
                            bone_app::EvidenceAvailability::Available { kind, title } => format!(
                                "{marker} {} · {}",
                                evidence_kind_label(*kind),
                                sanitize_external(title)
                            ),
                            bone_app::EvidenceAvailability::Private => {
                                format!("{marker} 私有来源（正文不可见）")
                            }
                            bone_app::EvidenceAvailability::Missing => {
                                format!("{marker} 来源暂不可用")
                            }
                        };
                        let keyboard_selected = state.focus == Focus::Detail
                            && state.focused_control == Some(HitTarget::Evidence(index));
                        ListItem::new(label).style(if keyboard_selected {
                            Style::default().fg(RAIL).bg(ACCENT)
                        } else if index == state.artifact.selection {
                            Style::default().fg(INK).bg(PANEL)
                        } else {
                            Style::default().fg(MUTED)
                        })
                    })
                    .collect::<Vec<_>>()
            }
            Some(page) if page.projection_pending => vec![
                ListItem::new("旧数据来源仍在有界回填；当前空列表不代表没有证据。")
                    .style(Style::default().fg(ATTENTION)),
            ],
            Some(_) => {
                vec![ListItem::new("这个结果没有显式引用来源。").style(Style::default().fg(MUTED))]
            }
            None => Vec::new(),
        };
        frame.render_widget(List::new(items), layout.body);
    }

    for region in artifact_regions(panel, state) {
        let label = match region.target {
            HitTarget::RefreshArtifact => "[ 刷新产物 ]",
            HitTarget::NewerEvidence => "[ 较新来源 ]",
            HitTarget::OlderEvidence => "[ 更多来源 ]",
            HitTarget::CloseEvidenceSource => "[ 返回来源列表 ]",
            HitTarget::PreviousEvidenceSource => "[ 上一段 ]",
            HitTarget::MoreEvidenceSource => "[ 继续读取正文 ]",
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

fn evidence_window(state: &UiState, height: usize) -> (usize, usize) {
    let length = state
        .artifact
        .evidence
        .as_ref()
        .map_or(0, |page| page.items.len());
    if length == 0 || height == 0 {
        return (0, 0);
    }
    let selected = state.artifact.selection.min(length - 1);
    let start = selected.saturating_sub(height - 1);
    (start, (start + height).min(length))
}

fn evidence_kind_label(kind: bone_app::EvidenceSourceKind) -> &'static str {
    match kind {
        bone_app::EvidenceSourceKind::ToolResult => "工具结果",
        bone_app::EvidenceSourceKind::PublishedReport => "公开报告",
        bone_app::EvidenceSourceKind::Reply => "回复",
    }
}
