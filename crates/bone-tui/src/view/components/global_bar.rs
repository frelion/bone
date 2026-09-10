use crate::{
    layout::{HitRegion, HitTarget},
    state::MainView,
};
use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    widgets::{Block, Paragraph},
};

pub(in crate::view) struct GlobalBarProps<'a> {
    pub active: MainView,
    pub focused: bool,
    pub selection: usize,
    pub attention_count: usize,
    pub workspace: &'a str,
    pub navigation: &'a [HitRegion],
    pub ink: Color,
    pub muted: Color,
    pub accent: Color,
    pub background: Color,
}

pub(in crate::view) fn render(frame: &mut Frame<'_>, area: Rect, props: GlobalBarProps<'_>) {
    frame.render_widget(
        Block::default().style(Style::default().bg(props.background)),
        area,
    );
    let mut nav_right = area.x;
    let mut navigation_index = 0;
    for region in props.navigation {
        let (label, view) = match region.target {
            HitTarget::Workbench => ("工作台".to_owned(), MainView::Workbench),
            HitTarget::Sessions => ("会话".to_owned(), MainView::Sessions),
            HitTarget::Attention => (
                if props.attention_count == 0 {
                    "需要你".to_owned()
                } else {
                    format!("需要你 {}", props.attention_count)
                },
                MainView::Attention,
            ),
            HitTarget::Settings => ("设置".to_owned(), MainView::Settings),
            _ => continue,
        };
        nav_right = nav_right.max(region.area.right());
        let mut style = Style::default()
            .fg(if props.active == view {
                props.accent
            } else {
                props.ink
            })
            .bg(props.background);
        if props.active == view {
            style = style.add_modifier(Modifier::BOLD);
        }
        if props.focused && props.selection == navigation_index {
            style = style.add_modifier(Modifier::REVERSED | Modifier::BOLD);
        }
        frame.render_widget(
            Paragraph::new(label)
                .alignment(Alignment::Center)
                .style(style),
            region.area,
        );
        navigation_index += 1;
    }
    frame.render_widget(
        Paragraph::new(props.workspace)
            .style(Style::default().fg(props.muted).bg(props.background)),
        Rect::new(
            nav_right,
            area.y,
            area.right().saturating_sub(nav_right),
            area.height,
        ),
    );
}

#[cfg(test)]
mod tests {
    use ratatui::{Terminal, backend::TestBackend};

    use super::*;

    #[test]
    fn navigation_is_drawn_in_the_exact_supplied_hit_rectangles() {
        let navigation = vec![
            HitRegion {
                area: Rect::new(0, 0, 12, 2),
                target: HitTarget::Workbench,
            },
            HitRegion {
                area: Rect::new(12, 0, 12, 2),
                target: HitTarget::Sessions,
            },
        ];
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| {
                render(
                    frame,
                    Rect::new(0, 0, 80, 2),
                    GlobalBarProps {
                        active: MainView::Workbench,
                        focused: true,
                        selection: 1,
                        attention_count: 0,
                        workspace: "workspace",
                        navigation: &navigation,
                        ink: Color::White,
                        muted: Color::DarkGray,
                        accent: Color::Green,
                        background: Color::Black,
                    },
                );
            })
            .unwrap();
        assert_eq!(navigation[0].area.right(), navigation[1].area.x);
        assert_eq!(navigation[1].area.right(), 24);
    }
}
