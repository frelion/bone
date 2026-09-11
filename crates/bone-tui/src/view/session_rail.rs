use crate::{
    layout::LayoutPlan,
    state::{SessionStatus, UiState},
    ui::theme,
    view::{ACCENT, DANGER, INK, MUTED, RAIL, SELECTED, single_line_external},
};
use bone_app::{InputState, RuntimeState};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Padding, Paragraph},
};

// A single status projection drives both row height and rendering.
pub(super) fn status(state: &UiState, index: usize) -> Option<(&'static str, Color)> {
    let info = &state.sessions[index];
    if let Some(snapshot) = state
        .session_ui
        .get(&info.id)
        .and_then(|ui| ui.snapshot.as_ref())
    {
        if let Some(problem) = &snapshot.problem {
            return Some(problem_status(problem));
        }
        for input in snapshot.inputs.iter().rev() {
            match input.state {
                InputState::WaitingForUser { .. } => return Some(("Needs your answer", ACCENT)),
                InputState::RoutingFailed { .. } | InputState::Rejected { .. } => {
                    return Some(("Failed", DANGER));
                }
                _ => {}
            }
        }
        match snapshot.runtime {
            RuntimeState::Running { .. } => {}
            RuntimeState::Starting => return Some(("Starting", MUTED)),
            RuntimeState::Closing { .. } => return Some(("Stopping", MUTED)),
            RuntimeState::Detached => {}
        }
    }
    if state
        .session_ui
        .get(&info.id)
        .is_some_and(|ui| ui.working())
    {
        return (state.selected != Some(info.id)).then_some(("Working", MUTED));
    }
    match state.session_statuses.get(&info.id) {
        Some(SessionStatus::NeedsAttention) => Some(("Needs you", ACCENT)),
        Some(SessionStatus::Recoverable) => Some(("Can resume", MUTED)),
        _ => None,
    }
}

pub(super) fn render(frame: &mut Frame<'_>, plan: &LayoutPlan, state: &UiState) {
    let Some(area) = plan.session_rail else {
        return;
    };
    frame.render_widget(Block::default().style(Style::default().bg(RAIL)), area);
    let project = state
        .workspace
        .as_ref()
        .map(|(_, name)| single_line_external(name))
        .unwrap_or_else(|| "BONE".into());
    frame.render_widget(
        Paragraph::new(project).style(theme::label(INK)),
        Rect::new(area.x + 3, area.y + 1, area.width.saturating_sub(6), 1),
    );
    let spacious = crate::layout::comfortable(area);
    frame.render_widget(
        Paragraph::new("+ New session")
            .style(Style::default().fg(INK))
            .block(
                Block::default()
                    .style(Style::default().bg(super::INPUT))
                    .padding(Padding::new(2, 1, u16::from(spacious), u16::from(spacious))),
            ),
        crate::layout::new_session_area(area),
    );
    for row in &plan.session_rows {
        let index = row.index;
        let info = &state.sessions[index];
        let selected = state.selected == Some(info.id);
        let highlighted = if state.focus == crate::state::Focus::Sessions {
            state.session_candidate.or(state.selected) == Some(info.id)
        } else {
            selected
        };
        let draft = state.session_ui.get(&info.id).map_or(
            state.session_statuses.get(&info.id) == Some(&SessionStatus::Draft),
            |ui| !ui.draft.is_empty(),
        );
        let mut title = single_line_external(&info.title);
        if draft {
            title = format!("· {title}");
        }
        let mut lines = vec![Line::from(vec![
            Span::raw("  "),
            Span::styled(
                title,
                if selected {
                    theme::label(INK)
                } else {
                    theme::body(INK)
                },
            ),
        ])];
        if let Some((label, tone)) = status(state, index) {
            lines.push(Line::styled(
                format!("  {label}"),
                Style::default().fg(tone),
            ));
        }
        if highlighted {
            frame.render_widget(
                Block::default().style(Style::default().bg(SELECTED)),
                row.area,
            );
        }
        frame.render_widget(
            Paragraph::new(lines).block(Block::default().padding(Padding::new(
                0,
                1,
                u16::from(spacious),
                u16::from(spacious),
            ))),
            row.area,
        );
        if selected {
            super::marks::paint(
                frame,
                row.area,
                ACCENT,
                if highlighted { SELECTED } else { RAIL },
            );
        }
    }
    if state.focus == crate::state::Focus::Sessions {
        frame.render_widget(
            Paragraph::new("↑↓ browse · enter open").style(Style::default().fg(MUTED)),
            Rect::new(
                area.x + 1,
                area.bottom().saturating_sub(1),
                area.width.saturating_sub(2),
                1,
            ),
        );
    }
}

pub(super) fn problem_status(problem: &bone_app::AppProblem) -> (&'static str, Color) {
    use bone_app::AppProblem::*;
    match problem {
        Configuration(_) => ("Needs configuration", ACCENT),
        LoginRequired(_) => ("Needs login", ACCENT),
        ProfileBusy(_) => ("Profile busy", ACCENT),
        Provider(_) => ("Provider error", DANGER),
        Storage(_) => ("Storage error", DANGER),
        Tools(_) => ("Tool error", DANGER),
        Agent(_) => ("Agent error", DANGER),
    }
}
