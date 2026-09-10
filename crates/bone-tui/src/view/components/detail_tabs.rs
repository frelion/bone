use crate::{
    layout::{HitRegion, HitTarget},
    state::DetailKind,
};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    widgets::Paragraph,
};

pub(in crate::view) struct DetailTabsProps {
    pub active: DetailKind,
    pub focused: bool,
    pub selection: usize,
    pub active_color: Color,
    pub inactive_color: Color,
    pub background: Color,
}

pub(in crate::view) fn regions(area: Rect) -> Vec<HitRegion> {
    if area.width == 0 || area.height < 4 {
        return Vec::new();
    }
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Ratio(1, 6),
            Constraint::Ratio(1, 6),
            Constraint::Ratio(1, 6),
            Constraint::Ratio(1, 6),
            Constraint::Ratio(1, 6),
            Constraint::Ratio(1, 6),
        ])
        .split(Rect::new(area.x, area.y.saturating_add(2), area.width, 2))
        .iter()
        .copied()
        .zip([
            HitTarget::DetailWork,
            HitTarget::DetailChanges,
            HitTarget::DetailContext,
            HitTarget::DetailArtifacts,
            HitTarget::DetailRecords,
            HitTarget::DetailAcceptance,
        ])
        .map(|(area, target)| HitRegion { area, target })
        .collect()
}

pub(in crate::view) fn render(frame: &mut Frame<'_>, area: Rect, props: DetailTabsProps) {
    for (index, region) in regions(area).into_iter().enumerate() {
        let (label, kind) = match region.target {
            HitTarget::DetailWork => ("工作", DetailKind::Work),
            HitTarget::DetailChanges => ("变更", DetailKind::Changes),
            HitTarget::DetailContext => ("上下文", DetailKind::Context),
            HitTarget::DetailArtifacts => ("产物", DetailKind::Artifacts),
            HitTarget::DetailRecords => ("记录", DetailKind::Records),
            HitTarget::DetailAcceptance => ("验收", DetailKind::Acceptance),
            _ => continue,
        };
        let mut style = Style::default()
            .fg(if props.active == kind {
                props.active_color
            } else {
                props.inactive_color
            })
            .bg(props.background);
        if props.active == kind {
            style = style.add_modifier(Modifier::BOLD);
        }
        if props.focused && props.selection == index {
            style = style.add_modifier(Modifier::REVERSED | Modifier::BOLD);
        }
        frame.render_widget(
            Paragraph::new(label)
                .alignment(Alignment::Center)
                .style(style),
            region.area,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn six_tabs_share_the_rendered_row_without_overlap() {
        let area = Rect::new(7, 3, 80, 20);
        let hits = regions(area);
        assert_eq!(hits.len(), 6);
        assert_eq!(hits[0].area, Rect::new(7, 5, 13, 2));
        assert_eq!(hits[5].area.right(), area.right());
        assert!(
            hits.windows(2)
                .all(|pair| pair[0].area.right() == pair[1].area.x)
        );
    }
}
