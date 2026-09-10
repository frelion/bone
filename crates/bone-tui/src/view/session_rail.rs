use bone_app::RuntimeState;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Padding, Paragraph},
};

use crate::{
    state::{Focus, SessionStatus, UiState},
    view::{ACCENT, INK, MUTED, RAIL, single_line_external},
};

pub(super) fn render(frame: &mut Frame<'_>, area: Rect, start: usize, state: &UiState) {
    let focused = state.focus == Focus::Sessions;
    frame.render_widget(
        Block::default()
            .borders(Borders::RIGHT)
            .border_style(Style::default().fg(if focused {
                ACCENT
            } else {
                Color::Rgb(32, 36, 41)
            }))
            .style(Style::default().bg(RAIL)),
        area,
    );
    let header = Rect::new(area.x, area.y, area.width, 3.min(area.height));
    frame.render_widget(
        Paragraph::new(Line::styled(
            "SESSIONS",
            Style::default().fg(if focused { INK } else { MUTED }),
        ))
        .style(Style::default().bg(RAIL))
        .block(Block::default().padding(Padding::new(2, 1, 1, 0))),
        header,
    );

    let list_bottom = area.bottom().saturating_sub(1);
    let mut y = header.bottom();
    for session in state.sessions.iter().skip(start) {
        if y.saturating_add(2) > list_bottom {
            break;
        }
        let selected = state.selected == Some(session.id);
        let runtime = state
            .session_ui
            .get(&session.id)
            .and_then(|ui| ui.snapshot.as_ref())
            .map(|snapshot| runtime_label(&snapshot.runtime))
            .unwrap_or_else(|| {
                if session.archived {
                    "Archived"
                } else {
                    match state
                        .session_statuses
                        .get(&session.id)
                        .copied()
                        .unwrap_or(SessionStatus::Ready)
                    {
                        SessionStatus::Ready => "Ready",
                        SessionStatus::Draft => "Draft",
                        SessionStatus::NeedsAttention => "Needs you",
                        SessionStatus::Recoverable => "Can resume",
                    }
                }
            });
        let unread = state.session_ui.get(&session.id).map_or(0, |ui| ui.unread);
        let status = if unread > 0 {
            format!("{runtime} · {unread} new")
        } else {
            runtime.into()
        };
        let selected_bg = if focused {
            Color::Rgb(35, 43, 51)
        } else {
            Color::Rgb(29, 34, 40)
        };
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(vec![
                    Span::styled(
                        if selected { "│ " } else { "  " },
                        Style::default().fg(ACCENT),
                    ),
                    Span::styled(
                        single_line_external(&session.title),
                        Style::default()
                            .fg(if selected { INK } else { MUTED })
                            .add_modifier(if selected {
                                Modifier::BOLD
                            } else {
                                Modifier::empty()
                            }),
                    ),
                ]),
                Line::styled(format!("  {status}"), Style::default().fg(MUTED)),
            ])
            .style(Style::default().bg(if selected { selected_bg } else { RAIL })),
            Rect::new(area.x, y, area.width, 2),
        );
        y += 2;
    }
    if area.height > 1 {
        let workspace = state
            .workspace
            .as_ref()
            .map(|(_, name)| single_line_external(name))
            .unwrap_or_else(|| "Opening workspace".into());
        frame.render_widget(
            Paragraph::new(workspace)
                .style(Style::default().fg(MUTED).bg(RAIL))
                .block(Block::default().padding(Padding::horizontal(2))),
            Rect::new(area.x, area.bottom() - 1, area.width, 1),
        );
    }
}

fn runtime_label(runtime: &RuntimeState) -> &'static str {
    match runtime {
        RuntimeState::Detached => "Ready",
        RuntimeState::Starting => "Starting",
        RuntimeState::Running { .. } => "Working",
        RuntimeState::Closing { .. } => "Stopping",
    }
}
