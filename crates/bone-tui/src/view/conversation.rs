use bone_app::{ActivityKind, JobState, RuntimeState};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Padding, Paragraph, Wrap},
};

use crate::{
    layout::{LayoutMode, LayoutPlan, TranscriptMetrics},
    state::{CommandSpec, Focus, UiState},
    view::{
        ACCENT, ATTENTION, INK, MUTED, PANEL, composer, message, single_line_external,
        slash_palette,
    },
};

pub(super) fn render(
    frame: &mut Frame<'_>,
    plan: &LayoutPlan,
    state: &UiState,
    slash_matches: &[&CommandSpec],
) -> Option<TranscriptMetrics> {
    if plan.mode == LayoutMode::TooSmall {
        render_too_small(frame, plan.conversation.unwrap_or(plan.screen), state);
        return None;
    }
    let (Some(header), Some(transcript), Some(composer_area)) =
        (plan.session_header, plan.transcript, plan.composer)
    else {
        return None;
    };
    render_header(frame, header, state);
    let metrics = render_transcript(frame, transcript, state);
    composer::render(frame, composer_area, state);
    if let Some(area) = plan.slash_palette {
        slash_palette::render(frame, area, state, slash_matches);
    }
    metrics
}

fn render_too_small(frame: &mut Frame<'_>, area: Rect, state: &UiState) {
    let draft = state
        .selected_ui()
        .map_or(&state.orphan_draft, |ui| &ui.draft);
    frame.render_widget(
        Paragraph::new(format!(
            "Window too small ({}×{}). Minimum 40×12.{}",
            area.width,
            area.height,
            if draft.is_empty() {
                ""
            } else {
                " Draft preserved."
            }
        ))
        .style(Style::default().fg(ATTENTION))
        .wrap(Wrap { trim: false }),
        area,
    );
}

fn render_header(frame: &mut Frame<'_>, area: Rect, state: &UiState) {
    let (title, runtime) = state
        .selected_ui()
        .map_or(("New conversation", "Ready"), |session| {
            let runtime = session
                .snapshot
                .as_ref()
                .map(|snapshot| runtime_label(&snapshot.runtime))
                .unwrap_or("Loading");
            (session.info.title.as_str(), runtime)
        });
    let focused = state.focus == Focus::Conversation;
    let status = state.status.as_deref().map(single_line_external);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                single_line_external(title),
                Style::default().fg(INK).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  {}", status.as_deref().unwrap_or(runtime)),
                Style::default().fg(MUTED),
            ),
        ]))
        .style(Style::default().bg(PANEL))
        .block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(Style::default().fg(if focused {
                    ACCENT
                } else {
                    Color::Rgb(47, 54, 63)
                }))
                .padding(Padding::horizontal(2)),
        ),
        area,
    );
}

fn render_transcript(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &UiState,
) -> Option<TranscriptMetrics> {
    let inner = Rect::new(
        area.x.saturating_add(2),
        area.y.saturating_add(1),
        area.width.saturating_sub(4),
        area.height.saturating_sub(2),
    );
    let Some(session) = state.selected_ui() else {
        frame.render_widget(
            Paragraph::new("Start with a clear request.")
                .style(Style::default().fg(MUTED))
                .block(Block::default().padding(Padding::new(1, 1, 1, 0))),
            inner,
        );
        return None;
    };

    let mut rows = Vec::<Line<'static>>::new();
    let mut event_rows = std::collections::BTreeMap::new();
    for entry in &session.history {
        let rendered = message::rows(&entry.event, inner.width);
        if rendered.is_empty() {
            continue;
        }
        let mut row_count = rendered.len();
        if !rows.is_empty() {
            rows.push(Line::default());
            row_count += 1;
        }
        rows.extend(rendered);
        event_rows.insert(entry.sequence, row_count);
    }

    if session.scroll_from_tail == 0
        && let Some(snapshot) = &session.snapshot
    {
        let mut ephemeral = Vec::new();
        let limit = usize::from(inner.height);
        for job in snapshot.jobs.iter().rev() {
            if ephemeral.len() >= limit {
                break;
            }
            if matches!(job.state, JobState::Running | JobState::Waiting(_)) {
                ephemeral.push(message::compact("↳", &job.goal, ATTENTION));
            }
        }
        for activity in snapshot.activity.iter().rev() {
            if ephemeral.len() >= limit {
                break;
            }
            let name = match &activity.kind {
                ActivityKind::Coordinate => "Coordinating",
                ActivityKind::Work => "Working",
                ActivityKind::Compact => "Compacting context",
                ActivityKind::Tool { name } => name,
            };
            let body = activity.progress.as_deref().map_or_else(
                || single_line_external(name),
                |progress| {
                    format!(
                        "{}  {}",
                        single_line_external(name),
                        single_line_external(progress)
                    )
                },
            );
            ephemeral.push(message::compact("·", &body, MUTED));
        }
        ephemeral.reverse();
        if !ephemeral.is_empty() && !rows.is_empty() {
            rows.push(Line::default());
        }
        rows.extend(ephemeral);
    }

    if rows.is_empty() {
        frame.render_widget(
            Paragraph::new("Start with a clear request.").style(Style::default().fg(MUTED)),
            inner,
        );
        return Some(TranscriptMetrics {
            total_rows: 0,
            viewport_rows: usize::from(inner.height),
            event_rows,
        });
    }
    let viewport = usize::from(inner.height);
    let visible = visible_rows(&rows, viewport, session.scroll_from_tail);
    let top_padding = viewport.saturating_sub(visible.len()) as u16;
    frame.render_widget(
        Paragraph::new(visible),
        Rect::new(
            inner.x,
            inner.y.saturating_add(top_padding),
            inner.width,
            inner.height.saturating_sub(top_padding),
        ),
    );
    Some(TranscriptMetrics {
        total_rows: rows.len(),
        viewport_rows: viewport,
        event_rows,
    })
}

fn visible_rows(
    rows: &[Line<'static>],
    viewport: usize,
    scroll_from_tail: usize,
) -> Vec<Line<'static>> {
    let end = rows.len().saturating_sub(scroll_from_tail.min(rows.len()));
    rows[end.saturating_sub(viewport)..end].to_vec()
}

fn runtime_label(runtime: &RuntimeState) -> &'static str {
    match runtime {
        RuntimeState::Detached => "Ready",
        RuntimeState::Starting => "Starting",
        RuntimeState::Running { .. } => "Working",
        RuntimeState::Closing { .. } => "Stopping",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_row_of_a_long_message_is_reachable() {
        let rows = (0..40)
            .map(|index| Line::from(index.to_string()))
            .collect::<Vec<_>>();
        assert_eq!(visible_rows(&rows, 10, 0)[0].to_string(), "30");
        assert_eq!(visible_rows(&rows, 10, 30)[0].to_string(), "0");
    }
}
