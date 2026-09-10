use super::chrome::{ActionButton, render_action_button};
use crate::layout::{HitRegion, HitTarget};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    widgets::Paragraph,
};

pub(in crate::view) struct ActionBarProps<'a> {
    pub message: &'a str,
    pub is_status: bool,
    pub actions: &'a [HitRegion],
    pub selected_target: Option<HitTarget>,
    pub ink: Color,
    pub muted: Color,
    pub danger: Color,
    pub background: Color,
}

pub(in crate::view) fn render(frame: &mut Frame<'_>, area: Rect, props: ActionBarProps<'_>) {
    frame.render_widget(
        Paragraph::new(props.message).style(
            Style::default()
                .fg(if props.is_status {
                    props.danger
                } else {
                    props.muted
                })
                .bg(props.background),
        ),
        area,
    );
    for region in props.actions {
        let label = match region.target {
            HitTarget::Submit => "[ 发送 ]",
            HitTarget::Stop => "[ 停止 ]",
            HitTarget::Quit => "[ 退出 ]",
            HitTarget::Acceptance => "[ 查看详情 ]",
            _ => continue,
        };
        let selected = props.selected_target == Some(region.target);
        render_action_button(
            frame,
            region.area,
            ActionButton {
                label,
                foreground: if selected {
                    props.background
                } else {
                    props.ink
                },
                background: if selected {
                    props.ink
                } else {
                    props.background
                },
                bold: true,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use ratatui::{Terminal, backend::TestBackend};

    use super::*;

    #[test]
    fn actions_are_rendered_in_caller_owned_hit_rectangles() {
        let actions = vec![HitRegion {
            area: Rect::new(68, 22, 12, 2),
            target: HitTarget::Submit,
        }];
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| {
                render(
                    frame,
                    Rect::new(0, 22, 80, 2),
                    ActionBarProps {
                        message: "status",
                        is_status: true,
                        actions: &actions,
                        selected_target: Some(HitTarget::Submit),
                        ink: Color::White,
                        muted: Color::DarkGray,
                        danger: Color::Red,
                        background: Color::Black,
                    },
                );
            })
            .unwrap();
        assert_eq!(actions[0].area.bottom(), 24);
        assert_eq!(actions[0].target, HitTarget::Submit);
    }
}
