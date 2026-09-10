use super::*;
pub(super) fn render_records(frame: &mut Frame<'_>, area: Rect, state: &UiState, title: &str) {
    let mut lines = vec![Line::styled(
        sanitize_external(title),
        Style::default().fg(INK).add_modifier(Modifier::BOLD),
    )];
    let Some(ui) = state.selected_ui() else {
        lines.push(Line::styled("没有选中的会话。", Style::default().fg(MUTED)));
        frame.render_widget(Paragraph::new(lines).block(section_block("详情")), area);
        return;
    };
    lines.push(Line::styled(
        "按 App 持久序号展示当前已加载的事实窗口；继续向上滚动会分页补读。",
        Style::default().fg(MUTED),
    ));
    let index = state.detail_scroll.min(ui.history.len().saturating_sub(1));
    if let Some(entry) = ui.history.iter().rev().nth(index) {
        lines.push(Line::styled(
            format!("#{} · {}/{}", entry.sequence.0, index + 1, ui.history.len()),
            Style::default().fg(ACCENT),
        ));
        let (label, body) = record_text(&entry.event);
        lines.push(Line::styled(label, Style::default().fg(MUTED)));
        lines.extend(components::paged_reader::visible_lines(
            body.as_ref(),
            0,
            area.width.saturating_sub(2),
            area.height.saturating_sub(6),
        ));
    }
    frame.render_widget(Paragraph::new(lines).block(section_block("详情")), area);
}

fn record_text(event: &bone_app::SessionEvent) -> (&'static str, std::borrow::Cow<'_, str>) {
    use bone_app::SessionEvent::*;
    match event {
        InputSubmitted { text, .. } => ("你", text.into()),
        Reply { text, .. } => ("BONE", text.into()),
        QuestionAsked { text, .. } => ("需要你", text.into()),
        InputRejected { message, .. } => ("未接受", message.into()),
        RoutingFailed { message, .. } => ("路由失败", message.into()),
        JobFinished { summary, .. } => ("工作结果", summary.into()),
        AcceptanceRecorded { reason, .. } => ("用户验收", reason.into()),
        WriteResolved { evidence, .. } => ("写入核查依据", evidence.into()),
        ToolFinished { tool, outcome, .. } => (
            "工具",
            format!(
                "{} · {}",
                tool,
                if outcome.result.is_ok() {
                    "完成"
                } else {
                    "失败"
                }
            )
            .into(),
        ),
        Interrupted { .. } => ("已中断", "旧执行已丢失，不会自动重放。".into()),
        InputAccepted { .. } => ("已接收", "要求已进入执行。".into()),
        InputCancelled { .. } => ("已取消", "要求已取消。".into()),
        RuntimeStarted { .. } => ("系统", "执行环境已启动。".into()),
        RuntimeReconfigured { .. } => ("系统", "运行配置已更新。".into()),
        RuntimeClosed { .. } => ("系统", "执行环境已关闭。".into()),
        InputFinished { .. } => ("请求结束", "请求已经结束。".into()),
    }
}
