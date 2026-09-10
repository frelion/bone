use super::*;

pub(super) fn render_workbench(frame: &mut Frame<'_>, area: Rect, state: &UiState) {
    let Some(session) = state.selected_ui() else {
        frame.render_widget(
            Paragraph::new(Text::from(vec![
                Line::styled(
                    "还没有会话",
                    Style::default().fg(INK).add_modifier(Modifier::BOLD),
                ),
                Line::raw(""),
                Line::styled("选择可见的“新建会话”开始工作。", Style::default().fg(MUTED)),
            ]))
            .alignment(Alignment::Center)
            .block(Block::default().padding(Padding::new(2, 2, 3, 1))),
            area,
        );
        return;
    };

    let composer = composer_area(area);
    let upper = Rect::new(
        area.x,
        area.y,
        area.width,
        area.height.saturating_sub(composer.height),
    );
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1)])
        .split(upper);
    let runtime = session
        .snapshot
        .as_ref()
        .map(|view| runtime_label(&view.runtime))
        .unwrap_or("正在载入");
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                single_line_external(&session.info.title),
                Style::default().fg(INK).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("   {runtime}"), Style::default().fg(MUTED)),
        ]))
        .block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(Style::default().fg(Color::DarkGray))
                .padding(Padding::horizontal(1)),
        ),
        rows[0],
    );

    let height = rows[1].height.saturating_sub(2) as usize;
    let history_end = session
        .history
        .len()
        .saturating_sub(session.scroll_from_tail.min(session.history.len()));
    let history_start = history_end.saturating_sub(height.saturating_mul(2).max(1));
    let mut timeline = Vec::with_capacity(height.saturating_mul(2));
    for entry in session.history.range(history_start..history_end) {
        timeline.extend(event_lines(&entry.event, rows[1].width.saturating_sub(2)));
    }
    if session.scroll_from_tail == 0
        && let Some(snapshot) = &session.snapshot
    {
        for activity in snapshot.activity.iter().rev().take(height).rev() {
            let name = match &activity.kind {
                ActivityKind::Coordinate => "协调",
                ActivityKind::Work => "工作",
                ActivityKind::Compact => "整理上下文",
                ActivityKind::Tool { name } => name,
            };
            timeline.push(Line::from(vec![
                Span::styled("● ", Style::default().fg(ACCENT)),
                Span::styled(single_line_external(name), Style::default().fg(MUTED)),
                Span::styled(
                    activity
                        .progress
                        .as_deref()
                        .map(|text| format!("  {}", single_line_external(text)))
                        .unwrap_or_default(),
                    Style::default().fg(MUTED),
                ),
            ]));
        }
        for job in snapshot.jobs.iter().rev().take(height).rev() {
            if matches!(job.state, JobState::Running | JobState::Waiting(_)) {
                timeline.push(Line::from(vec![
                    Span::styled("↳ ", Style::default().fg(ATTENTION)),
                    Span::styled(single_line_external(&job.goal), Style::default().fg(INK)),
                ]));
            }
        }
    }
    if timeline.is_empty() {
        timeline.push(Line::styled(
            "这段会话还没有消息。输入一条清晰要求即可开始。",
            Style::default().fg(MUTED),
        ));
    }
    let start = timeline.len().saturating_sub(height);
    let visible = timeline.split_off(start);
    frame.render_widget(
        Paragraph::new(visible)
            .wrap(Wrap { trim: false })
            .block(Block::default().padding(Padding::horizontal(1))),
        rows[1],
    );
    render_composer(frame, composer, state);
}

pub(super) fn render_composer(frame: &mut Frame<'_>, area: Rect, state: &UiState) {
    let session = state.selected_ui();
    let text = session.map_or("", |ui| ui.draft.as_str());
    let title = if session.is_some_and(|ui| ui.reply_to.is_some()) {
        " 回答选中的问题 · 发送会绑定目标 "
    } else {
        match session.and_then(|ui| ui.submitting.as_ref()) {
            Some(pending) if pending.failed => " 上一条提交状态未知；再次发送会安全重试 ",
            Some(_) => " 正在保存上一条要求；可继续编辑下一条 ",
            None => " 输入要求 ",
        }
    };
    let border = if state.focus == Focus::Composer {
        ACCENT
    } else {
        Color::DarkGray
    };
    frame.render_widget(
        Paragraph::new(sanitize_external(visible_tail(
            text,
            usize::from(area.width)
                .saturating_mul(usize::from(area.height))
                .saturating_mul(2)
                .max(1),
        )))
        .style(Style::default().fg(INK).bg(RAIL))
        .wrap(Wrap { trim: false })
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border))
                .padding(Padding::horizontal(1)),
        ),
        area,
    );
}
