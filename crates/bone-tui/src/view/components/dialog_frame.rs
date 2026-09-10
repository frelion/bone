use super::chrome::{ActionButton, render_action_button};
use crate::layout::{HitRegion, HitTarget, dialog_confirm_area};
use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Color, Style},
    widgets::{Block, Borders, Clear, Padding, Paragraph, Wrap},
};

pub(in crate::view) struct DialogFrameProps<'a> {
    pub title: &'a str,
    pub body: String,
    pub accent: Color,
    pub foreground: Color,
    pub background: Color,
}

pub(in crate::view) fn render(
    frame: &mut Frame<'_>,
    area: Rect,
    props: DialogFrameProps<'_>,
) -> HitRegion {
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(props.body)
            .style(Style::default().fg(props.foreground).bg(props.background))
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .title(props.title)
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(props.accent))
                    .padding(Padding::uniform(1)),
            ),
        area,
    );
    let confirm = dialog_confirm_area(area);
    render_action_button(
        frame,
        confirm,
        ActionButton {
            label: "[ 确认 ]",
            foreground: props.accent,
            background: props.background,
            bold: true,
        },
    );
    HitRegion {
        area: confirm,
        target: HitTarget::DialogConfirm,
    }
}

#[cfg(test)]
mod tests {
    use ratatui::{Terminal, backend::TestBackend};

    use super::*;

    #[test]
    fn confirmation_hit_is_derived_from_the_drawn_dialog() {
        let area = Rect::new(10, 4, 60, 13);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let mut hit = None;
        terminal
            .draw(|frame| {
                hit = Some(render(
                    frame,
                    area,
                    DialogFrameProps {
                        title: "确认",
                        body: "正文".into(),
                        accent: Color::Green,
                        foreground: Color::White,
                        background: Color::Black,
                    },
                ));
            })
            .unwrap();
        let hit = hit.unwrap();
        assert_eq!(hit.target, HitTarget::DialogConfirm);
        assert_eq!(hit.area, dialog_confirm_area(area));
    }
}
