use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Padding, Paragraph},
};

/// Shared visual container for product sections. It is deliberately stateless:
/// pages own selection and behavior, while the component only draws chrome.
pub(in crate::view) fn section_block(title: &str) -> Block<'_> {
    Block::default()
        .title(format!(" {title} "))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .padding(Padding::uniform(1))
}

#[derive(Clone, Copy)]
pub(in crate::view) struct ActionButton<'a> {
    pub label: &'a str,
    pub foreground: Color,
    pub background: Color,
    pub bold: bool,
}

/// Draw a visible action in a caller-owned rectangle. Hit regions remain owned
/// by the composing page and are calculated from this same rectangle.
pub(in crate::view) fn render_action_button(
    frame: &mut Frame<'_>,
    area: Rect,
    props: ActionButton<'_>,
) {
    let mut style = Style::default().fg(props.foreground).bg(props.background);
    if props.bold {
        style = style.add_modifier(Modifier::BOLD);
    }
    frame.render_widget(
        Paragraph::new(props.label)
            .alignment(ratatui::layout::Alignment::Center)
            .style(style),
        area,
    );
}
