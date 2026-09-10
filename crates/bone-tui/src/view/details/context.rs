use super::*;
pub(super) fn render_context(frame: &mut Frame<'_>, area: Rect, state: &UiState, title: &str) {
    let mut lines = vec![Line::styled(
        sanitize_external(title),
        Style::default().fg(INK).add_modifier(Modifier::BOLD),
    )];
    let Some(snapshot) = state.selected_ui().and_then(|ui| ui.snapshot.as_ref()) else {
        lines.push(Line::styled(
            "正在从 App 读取会话上下文…",
            Style::default().fg(MUTED),
        ));
        frame.render_widget(Paragraph::new(lines).block(section_block("详情")), area);
        return;
    };
    lines.push(Line::styled(
        "这里只展示 App 公开的用户要求；Agent 私有规划不会被伪装成公共上下文。",
        Style::default().fg(MUTED),
    ));
    if let Some(input) = snapshot.inputs.get(
        state
            .detail_scroll
            .min(snapshot.inputs.len().saturating_sub(1)),
    ) {
        lines.push(Line::styled(
            format!(
                "要求 {} · {} · {}/{}",
                input.id.0,
                input_state_label(&input.state),
                state.detail_scroll.min(snapshot.inputs.len() - 1) + 1,
                snapshot.inputs.len()
            ),
            Style::default().fg(ACCENT),
        ));
        lines.extend(components::paged_reader::visible_lines(
            &input.text,
            0,
            area.width.saturating_sub(2),
            area.height.saturating_sub(7),
        ));
    } else {
        lines.push(Line::styled(
            "当前没有公开的用户要求。",
            Style::default().fg(MUTED),
        ));
    }
    if let Some(problem) = &snapshot.problem {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            format!("当前阻塞：{}", app_problem_label(problem)),
            Style::default().fg(DANGER),
        ));
    }
    frame.render_widget(Paragraph::new(lines).block(section_block("详情")), area);
}

pub(super) fn input_state_label(state: &bone_app::InputState) -> &'static str {
    match state {
        bone_app::InputState::Queued { .. } => "已排队",
        bone_app::InputState::Posting { .. } => "提交中",
        bone_app::InputState::Accepted { .. } => "执行中",
        bone_app::InputState::WaitingForUser { .. } => "等待回答",
        bone_app::InputState::RoutingFailed { .. } => "路由失败",
        bone_app::InputState::Finished { .. } => "已完成",
        bone_app::InputState::Rejected { .. } => "已拒绝",
        bone_app::InputState::Cancelled => "已取消",
        bone_app::InputState::Interrupted { .. } => "已中断",
    }
}

pub(super) fn app_problem_label(problem: &bone_app::AppProblem) -> String {
    match problem {
        bone_app::AppProblem::Configuration(_) => "运行配置尚未完整".into(),
        bone_app::AppProblem::LoginRequired(profile) => format!("连接 {profile} 需要登录"),
        bone_app::AppProblem::ProfileBusy(profile) => format!("连接 {profile} 正被其他操作使用"),
        bone_app::AppProblem::Provider(message)
        | bone_app::AppProblem::Storage(message)
        | bone_app::AppProblem::Tools(message)
        | bone_app::AppProblem::Agent(message) => single_line_external(message),
    }
}
