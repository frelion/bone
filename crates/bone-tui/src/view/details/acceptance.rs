use super::*;
pub(super) fn render_acceptance(
    frame: &mut Frame<'_>,
    area: Rect,
    panel: Rect,
    state: &UiState,
    title: &str,
) {
    let results = state
        .selected
        .and_then(|session| state.results.get(&session));
    let can_accept = state.acceptance_target().is_some();
    let navigation = acceptance_navigation_regions(panel, state);
    let reserved = usize::from(can_accept) * 4 + usize::from(!navigation.is_empty()) * 2;
    let content = Rect::new(
        area.x,
        area.y,
        area.width,
        area.height.saturating_sub(reserved as u16),
    );
    let mut lines = vec![Line::styled(
        sanitize_external(title),
        Style::default().fg(INK).add_modifier(Modifier::BOLD),
    )];
    match results {
        Some(page) if !page.items.is_empty() => {
            let visible_items = usize::from(content.height.saturating_sub(3) / 3).max(1);
            for result in page
                .items
                .iter()
                .rev()
                .skip(state.detail_scroll)
                .take(visible_items)
            {
                lines.push(Line::styled(
                    format!("结果版本 {}", result.result.version.0),
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                ));
                lines.extend(
                    components::paged_reader::visible_lines(
                        &result.summary,
                        0,
                        content.width.saturating_sub(2),
                        1,
                    )
                    .into_iter()
                    .map(|line| line.style(Style::default().fg(INK))),
                );
                lines.push(Line::styled(
                    format!("仍需处理：{}", result.remaining.len()),
                    Style::default().fg(if result.remaining.is_empty() {
                        MUTED
                    } else {
                        ATTENTION
                    }),
                ));
            }
            if state.detail_scroll == 0 {
                let latest = page.items.last().expect("non-empty result page");
                lines.insert(
                    1,
                    Line::styled(
                        format!("当前判定对象：最新结果版本 {}", latest.result.version.0),
                        Style::default().fg(ATTENTION),
                    ),
                );
                if let Some(records) = state.acceptances.get(&latest.result) {
                    for record in records.items.iter().rev().take(2) {
                        let prefix = format!("{} · ", acceptance_decision_label(record.decision));
                        let remaining = content.width.saturating_sub(prefix.chars().count() as u16);
                        let reason = components::paged_reader::visible_lines(
                            &record.reason,
                            0,
                            remaining.max(1),
                            1,
                        )
                        .into_iter()
                        .next()
                        .unwrap_or_default();
                        lines.push(Line::from(vec![
                            Span::styled(prefix, Style::default().fg(MUTED)),
                            Span::styled(reason.to_string(), Style::default().fg(INK)),
                        ]));
                    }
                }
            }
        }
        Some(page) if page.projection_pending => lines.push(Line::styled(
            "旧数据的结果索引仍在有界回填。",
            Style::default().fg(ATTENTION),
        )),
        _ => lines.push(Line::styled(
            "当前还没有可验收的持久结果。",
            Style::default().fg(MUTED),
        )),
    }
    frame.render_widget(
        Paragraph::new(lines).block(section_block("结果与验收")),
        content,
    );
    if can_accept {
        for region in acceptance_action_regions(panel) {
            let (label, color) = match region.target {
                HitTarget::Accept => ("[ 接受 ]", ACCENT),
                HitTarget::PartiallyAccept => ("[ 部分接受 ]", ATTENTION),
                HitTarget::AcceptWithRisk => ("[ 带风险接受 ]", ATTENTION),
                HitTarget::Reject => ("[ 退回返工 ]", DANGER),
                _ => continue,
            };
            frame.render_widget(
                Paragraph::new(label).alignment(Alignment::Center).style(
                    if state.focus == Focus::Detail && state.focused_control == Some(region.target)
                    {
                        Style::default()
                            .fg(RAIL)
                            .bg(color)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                            .fg(color)
                            .bg(RAIL)
                            .add_modifier(Modifier::BOLD)
                    },
                ),
                region.area,
            );
        }
    }
    for region in navigation {
        let label = match region.target {
            HitTarget::NewerResults => "[ 较新结果 ]",
            HitTarget::OlderResults => "[ 更早结果 / 继续整理 ]",
            HitTarget::NewerAcceptances => "[ 较新判定 ]",
            HitTarget::OlderAcceptances => "[ 更早判定 ]",
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

pub(in crate::view) fn acceptance_action_regions(area: Rect) -> Vec<HitRegion> {
    if area.width == 0 || area.height < 4 {
        return Vec::new();
    }
    let controls = Rect::new(area.x, area.bottom().saturating_sub(4), area.width, 4);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Length(2)])
        .split(controls);
    let mut regions = Vec::with_capacity(4);
    for (row, targets) in [
        (rows[0], [HitTarget::Accept, HitTarget::PartiallyAccept]),
        (rows[1], [HitTarget::AcceptWithRisk, HitTarget::Reject]),
    ] {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(row);
        regions.push(HitRegion {
            area: columns[0],
            target: targets[0],
        });
        regions.push(HitRegion {
            area: columns[1],
            target: targets[1],
        });
    }
    regions
}

pub(in crate::view) fn acceptance_navigation_regions(
    area: Rect,
    state: &UiState,
) -> Vec<HitRegion> {
    let Some(page) = state
        .selected
        .and_then(|session| state.results.get(&session))
    else {
        return Vec::new();
    };
    let newer_results = state
        .selected_ui()
        .is_some_and(|ui| !ui.result_pages.back.is_empty());
    let show_results = page.older_cursor.is_some()
        || page.projection_pending
        || state
            .selected_ui()
            .is_some_and(|ui| ui.results_window_stale);
    let latest = page.items.last().map(|result| result.result);
    let newer_acceptances = latest.is_some_and(|result| {
        state
            .acceptance_pages
            .get(&result)
            .is_some_and(|pages| !pages.back.is_empty())
    });
    let show_acceptances = latest.is_some_and(|result| {
        state.acceptance_windows_stale.contains(&result)
            || state
                .acceptances
                .get(&result)
                .is_some_and(|acceptances| acceptances.older_cursor.is_some())
    });
    let has_results = !page.items.is_empty();
    let required_height = if has_results { 6 } else { 2 };
    if (!newer_results && !show_results && !newer_acceptances && !show_acceptances)
        || area.height < required_height
    {
        return Vec::new();
    }
    let y = area.bottom().saturating_sub(required_height);
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(25),
            Constraint::Percentage(25),
            Constraint::Percentage(25),
            Constraint::Percentage(25),
        ])
        .split(Rect::new(area.x, y, area.width, 2));
    let mut regions = Vec::with_capacity(4);
    if newer_results {
        regions.push(HitRegion {
            area: columns[0],
            target: HitTarget::NewerResults,
        });
    }
    if show_results {
        regions.push(HitRegion {
            area: columns[1],
            target: HitTarget::OlderResults,
        });
    }
    if newer_acceptances {
        regions.push(HitRegion {
            area: columns[2],
            target: HitTarget::NewerAcceptances,
        });
    }
    if show_acceptances {
        regions.push(HitRegion {
            area: columns[3],
            target: HitTarget::OlderAcceptances,
        });
    }
    regions
}

pub(super) fn acceptance_decision_label(decision: bone_app::AcceptanceDecision) -> &'static str {
    match decision {
        bone_app::AcceptanceDecision::Accepted => "接受",
        bone_app::AcceptanceDecision::PartiallyAccepted => "部分接受",
        bone_app::AcceptanceDecision::AcceptedWithRisk => "带风险接受",
        bone_app::AcceptanceDecision::Rejected => "退回返工",
    }
}
