use super::*;

pub(super) fn render_attention(frame: &mut Frame<'_>, area: Rect, state: &UiState) {
    if state.attention.is_empty() {
        frame.render_widget(
            Paragraph::new(if state.attention_projection_pending {
                "正在分批整理旧数据库的待处理索引；当前列表尚未宣称完整。"
            } else {
                "当前没有需要你处理的事项。"
            })
            .style(Style::default().fg(MUTED))
            .block(section_block("需要你")),
            area,
        );
        return;
    }
    let items =
        state.attention.iter().enumerate().map(|(index, item)| {
            let text = match item {
                bone_app::AttentionItem::WaitingForUser { text, .. } => {
                    format!("需要回答  {}", sanitize_external(text))
                }
                bone_app::AttentionItem::UnresolvedWrite { call, .. } => {
                    format!("核查未知写入  Call {}", call.id)
                }
            };
            ListItem::new(format!(
                "{} {text}",
                if index == state.attention_selection {
                    "›"
                } else {
                    " "
                }
            ))
            .style(Style::default().fg(ATTENTION).bg(
                if index == state.attention_selection {
                    Color::Rgb(38, 45, 53)
                } else {
                    PANEL
                },
            ))
        });
    let title = if state.attention_projection_pending {
        "需要你 · 旧数据仍在整理"
    } else {
        "需要你"
    };
    frame.render_widget(List::new(items).block(section_block(title)), area);
}

pub(super) fn attention_item_regions(area: Rect, state: &UiState) -> Vec<HitRegion> {
    let start = area.y.saturating_add(2);
    let bottom = area.bottom().saturating_sub(1);
    state
        .attention
        .iter()
        .enumerate()
        .filter_map(|(index, _)| {
            let y = start.saturating_add(index as u16);
            (y < bottom).then_some(HitRegion {
                area: Rect::new(area.x.saturating_add(1), y, area.width.saturating_sub(2), 1),
                target: HitTarget::AttentionItem(index),
            })
        })
        .collect()
}
