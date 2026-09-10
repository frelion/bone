use crate::layout::{HitRegion, HitTarget};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    widgets::{Block, List, ListItem, Padding, Paragraph},
};

pub(in crate::view) struct SessionRailItem {
    pub label: String,
    pub selected: bool,
}

pub(in crate::view) struct SessionRailProps {
    pub items: Vec<SessionRailItem>,
    pub ink: Color,
    pub muted: Color,
    pub accent: Color,
    pub background: Color,
    pub selected_background: Color,
}

pub(in crate::view) fn render(
    frame: &mut Frame<'_>,
    area: Rect,
    props: SessionRailProps,
) -> Vec<HitRegion> {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(2),
        ])
        .split(area);
    frame.render_widget(
        Paragraph::new("会话")
            .style(
                Style::default()
                    .fg(props.muted)
                    .bg(props.background)
                    .add_modifier(Modifier::BOLD),
            )
            .block(Block::default().padding(Padding::horizontal(1))),
        rows[0],
    );
    let count = props.items.len();
    frame.render_widget(
        List::new(props.items.into_iter().map(|item| {
            ListItem::new(item.label).style(if item.selected {
                Style::default().fg(props.ink).bg(props.selected_background)
            } else {
                Style::default().fg(props.muted).bg(props.background)
            })
        }))
        .style(Style::default().bg(props.background)),
        rows[1],
    );
    frame.render_widget(
        Paragraph::new(" ＋ 新建会话").style(
            Style::default()
                .fg(props.accent)
                .bg(props.background)
                .add_modifier(Modifier::BOLD),
        ),
        rows[2],
    );
    let mut hits = (0..count.min(usize::from(rows[1].height)))
        .map(|index| HitRegion {
            area: Rect::new(rows[1].x, rows[1].y + index as u16, rows[1].width, 1),
            target: HitTarget::Session(index),
        })
        .collect::<Vec<_>>();
    hits.push(HitRegion {
        area: rows[2],
        target: HitTarget::NewSession,
    });
    hits
}

#[cfg(test)]
mod tests {
    use ratatui::{Terminal, backend::TestBackend};

    use super::*;

    #[test]
    fn visible_rows_and_new_session_control_own_their_hit_rectangles() {
        let area = Rect::new(0, 2, 24, 36);
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        let mut hits = Vec::new();
        terminal
            .draw(|frame| {
                hits = render(
                    frame,
                    area,
                    SessionRailProps {
                        items: vec![
                            SessionRailItem {
                                label: "▌ first".into(),
                                selected: true,
                            },
                            SessionRailItem {
                                label: "  second".into(),
                                selected: false,
                            },
                        ],
                        ink: Color::White,
                        muted: Color::DarkGray,
                        accent: Color::Green,
                        background: Color::Black,
                        selected_background: Color::Gray,
                    },
                );
            })
            .unwrap();
        assert_eq!(hits[0].target, HitTarget::Session(0));
        assert_eq!(hits[0].area, Rect::new(0, 4, 24, 1));
        assert_eq!(hits[1].target, HitTarget::Session(1));
        assert_eq!(hits.last().unwrap().target, HitTarget::NewSession);
        assert_eq!(hits.last().unwrap().area.bottom(), area.bottom());
    }
}
