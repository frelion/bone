use bone_app::JobState;
use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph},
};
use unicode_width::UnicodeWidthStr;

use crate::{
    state::{Focus, UiState},
    view::{ACCENT, INK, MUTED, RAIL, primitives::editor_viewport, single_line_external},
};

pub(super) fn render(frame: &mut Frame<'_>, area: Rect, state: &UiState) {
    let session = state.selected_ui();
    let (draft, cursor) = session
        .map_or((state.orphan_draft.as_str(), state.orphan_cursor), |ui| {
            (ui.draft.as_str(), ui.draft_cursor)
        });
    let focused = state.focus == Focus::Composer;
    frame.render_widget(
        Block::default()
            .borders(Borders::LEFT)
            .border_style(Style::default().fg(if focused { ACCENT } else { MUTED }))
            .style(Style::default().bg(RAIL)),
        area,
    );
    let input = Rect::new(
        area.x.saturating_add(2),
        area.y.saturating_add(1),
        area.width.saturating_sub(3),
        area.height.saturating_sub(3),
    );
    let (value, cursor_x, cursor_y) = editor_viewport(draft, cursor, input.width, input.height);
    let text = if draft.is_empty() {
        Text::styled("Type a message…", Style::default().fg(MUTED))
    } else {
        Text::styled(value, Style::default().fg(INK))
    };
    frame.render_widget(Paragraph::new(text).style(Style::default().bg(RAIL)), input);

    let busy = session
        .and_then(|ui| ui.snapshot.as_ref())
        .is_some_and(|snapshot| {
            snapshot
                .jobs
                .iter()
                .any(|job| matches!(job.state, JobState::Running | JobState::Waiting(_)))
        });
    let footer = Rect::new(
        area.x.saturating_add(2),
        area.bottom().saturating_sub(1),
        area.width.saturating_sub(3),
        1,
    );
    let model = state
        .model_label
        .as_deref()
        .map(single_line_external)
        .unwrap_or_default();
    let action = if busy { "esc stop" } else { "enter send" };
    let hints = format!("shift+enter newline  {action}  / commands");
    let spacing = usize::from(footer.width)
        .saturating_sub(UnicodeWidthStr::width(model.as_str()))
        .saturating_sub(UnicodeWidthStr::width(hints.as_str()));
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(model, Style::default().fg(MUTED)),
            Span::raw(" ".repeat(spacing)),
            Span::styled("shift+enter newline  ", Style::default().fg(MUTED)),
            Span::styled(
                action,
                Style::default().fg(if focused { ACCENT } else { MUTED }),
            ),
            Span::styled("  / commands", Style::default().fg(MUTED)),
        ]))
        .style(Style::default().bg(RAIL)),
        footer,
    );
    if focused && input.width > 0 && input.height > 0 {
        frame.set_cursor_position((input.x + cursor_x, input.y + cursor_y));
    }
}
