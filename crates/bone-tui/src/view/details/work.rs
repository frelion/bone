use super::*;
pub(super) fn render_write_decision(
    frame: &mut Frame<'_>,
    area: Rect,
    panel_area: Rect,
    state: &UiState,
) {
    let write = state.attention_detail.as_ref().and_then(|item| match item {
        bone_app::AttentionItem::UnresolvedWrite { session, call, .. } => state
            .unresolved_writes
            .iter()
            .find(|write| write.session == *session && write.call == *call),
        _ => None,
    });
    let mut lines = vec![Line::styled(
        "未知写入核查",
        Style::default().fg(ATTENTION).add_modifier(Modifier::BOLD),
    )];
    if let Some(write) = write {
        lines.push(Line::styled(
            format!("工具  {}", sanitize_external(&write.tool)),
            Style::default().fg(INK),
        ));
        lines.push(Line::styled(
            format!("调用  {}", write.call.id),
            Style::default().fg(MUTED),
        ));
        lines.push(Line::raw(""));
        lines.push(Line::styled("参数", Style::default().fg(MUTED)));
        let arguments = bounded_json_preview(&write.arguments, 16 * 1024);
        lines.extend(components::paged_reader::visible_lines(
            &arguments,
            state.detail_scroll,
            content_width(area),
            area.height.saturating_sub(9),
        ));
        if let Some(outcome) = &write.outcome {
            lines.push(Line::raw(""));
            lines.push(Line::styled("已知结果", Style::default().fg(MUTED)));
            let outcome = outcome.result.as_ref().map_or_else(
                |error| visible_tail(&error.message, 16 * 1024).to_owned(),
                |value| bounded_json_preview(value, 16 * 1024),
            );
            lines.extend(components::paged_reader::visible_lines(
                &outcome,
                0,
                content_width(area),
                3,
            ));
        }
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "BONE 不会自动重放。请先在外部环境核查，再选择事实并填写依据。",
            Style::default().fg(ATTENTION),
        ));
    } else {
        lines.push(Line::styled(
            "该事项已经失效或详细记录尚未返回；不会提供核查按钮。",
            Style::default().fg(MUTED),
        ));
    }
    let content = Rect::new(area.x, area.y, area.width, area.height.saturating_sub(2));
    frame.render_widget(
        Paragraph::new(lines).block(section_block("核查依据")),
        content,
    );
    if write.is_some() {
        for region in write_resolution_regions(panel_area) {
            let (label, color) = match region.target {
                HitTarget::WriteApplied => (
                    if state.write_resolution_applied {
                        "› 确认已发生"
                    } else {
                        "[ 确认已发生 ]"
                    },
                    ATTENTION,
                ),
                HitTarget::WriteNotApplied => (
                    if state.write_resolution_applied {
                        "[ 确认未发生 ]"
                    } else {
                        "› 确认未发生"
                    },
                    ACCENT,
                ),
                _ => continue,
            };
            frame.render_widget(
                Paragraph::new(label).alignment(Alignment::Center).style(
                    Style::default()
                        .fg(color)
                        .bg(RAIL)
                        .add_modifier(Modifier::BOLD),
                ),
                region.area,
            );
        }
    }
}

fn bounded_json_preview(value: &serde_json::Value, limit: usize) -> String {
    struct LimitedWriter {
        bytes: Vec<u8>,
        limit: usize,
    }

    impl std::io::Write for LimitedWriter {
        fn write(&mut self, input: &[u8]) -> std::io::Result<usize> {
            let remaining = self.limit.saturating_sub(self.bytes.len());
            if remaining == 0 {
                return Err(std::io::Error::other("preview limit reached"));
            }
            if input.len() <= remaining {
                self.bytes.extend_from_slice(input);
                return Ok(input.len());
            }
            let mut end = remaining;
            while end > 0 && std::str::from_utf8(&input[..end]).is_err() {
                end -= 1;
            }
            self.bytes.extend_from_slice(&input[..end]);
            Err(std::io::Error::other("preview limit reached"))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let mut writer = LimitedWriter {
        bytes: Vec::with_capacity(limit.min(4096)),
        limit,
    };
    let truncated = serde_json::to_writer(&mut writer, value).is_err();
    let mut rendered = String::from_utf8(writer.bytes).unwrap_or_default();
    if truncated {
        rendered.push_str("…（预览已截断）");
    }
    rendered
}

pub(super) fn render_work(frame: &mut Frame<'_>, area: Rect, state: &UiState, title: &str) {
    let mut lines = vec![Line::styled(
        sanitize_external(title),
        Style::default().fg(INK).add_modifier(Modifier::BOLD),
    )];
    let Some(snapshot) = state.selected_ui().and_then(|ui| ui.snapshot.as_ref()) else {
        lines.push(Line::styled(
            "正在从 App 读取工作状态…",
            Style::default().fg(MUTED),
        ));
        frame.render_widget(Paragraph::new(lines).block(section_block("详情")), area);
        return;
    };
    let Some(job) = snapshot.jobs.get(
        state
            .detail_scroll
            .min(snapshot.jobs.len().saturating_sub(1)),
    ) else {
        lines.push(Line::styled(
            "当前没有工作分工。",
            Style::default().fg(MUTED),
        ));
        frame.render_widget(Paragraph::new(lines).block(section_block("详情")), area);
        return;
    };
    lines.push(Line::styled(
        format!(
            "Job {} · {} · {}/{}",
            job.id.id,
            job_state_label(&job.state),
            state.detail_scroll.min(snapshot.jobs.len() - 1) + 1,
            snapshot.jobs.len()
        ),
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
    ));
    for (label, body, color) in [
        ("目标", job.goal.as_str(), INK),
        ("范围", job.scope.as_str(), INK),
        ("完成条件", job.done_when.as_str(), MUTED),
    ] {
        lines.push(Line::styled(label, Style::default().fg(MUTED)));
        lines.extend(
            components::paged_reader::visible_lines(body, 0, content_width(area), 2)
                .into_iter()
                .map(|line| line.style(Style::default().fg(color))),
        );
    }
    if let Some(report) = &job.report {
        lines.push(Line::styled("报告", Style::default().fg(MUTED)));
        lines.extend(components::paged_reader::visible_lines(
            &report.summary,
            0,
            content_width(area),
            3,
        ));
    }
    frame.render_widget(Paragraph::new(lines).block(section_block("详情")), area);
}

fn content_width(area: Rect) -> u16 {
    area.width.saturating_sub(2).max(1)
}

pub(in crate::view) fn write_resolution_regions(area: Rect) -> Vec<HitRegion> {
    if area.height < 2 || area.width == 0 {
        return Vec::new();
    }
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(Rect::new(
            area.x,
            area.bottom().saturating_sub(2),
            area.width,
            2,
        ))
        .iter()
        .copied()
        .zip([HitTarget::WriteApplied, HitTarget::WriteNotApplied])
        .map(|(area, target)| HitRegion { area, target })
        .collect()
}

pub(super) fn job_state_label(state: &JobState) -> &'static str {
    match state {
        JobState::Ready => "待开始",
        JobState::Running => "进行中",
        JobState::Waiting(_) => "等待中",
        JobState::Paused => "已暂停",
        JobState::Finished { .. } => "已结束",
    }
}
