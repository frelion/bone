use crate::{
    layout::{HitRegion, HitTarget, LayoutPlan},
    state::{Focus, SessionStatus, UiState},
    ui::{
        focus,
        interaction::HitMap,
        theme::{self, DANGER, INK, MUTED, RAIL, SELECTED},
    },
    view::single_line_external,
};
use bone_app::{InputState, RuntimeState};
use ratatui::{
    Frame,
    layout::Rect,
    style::Color,
    widgets::{Block, Paragraph},
};
use std::time::{SystemTime, UNIX_EPOCH};

// Status is deliberately a compact semantic dot in the fixed three-line row;
// it must never displace the reply preview or message/time metadata.
pub(super) fn status_tone(state: &UiState, index: usize) -> Option<Color> {
    let info = &state.sessions[index];
    if let Some(snapshot) = state
        .session_ui
        .get(&info.id)
        .and_then(|ui| ui.snapshot.as_ref())
    {
        if let Some(problem) = &snapshot.problem {
            return Some(problem_status(problem).1);
        }
        for input in snapshot.inputs.iter().rev() {
            match input.state {
                InputState::WaitingForUser { .. } => {
                    return Some(theme::WARNING);
                }
                InputState::RoutingFailed { .. } | InputState::Rejected { .. } => {
                    return Some(DANGER);
                }
                _ => {}
            }
        }
        match snapshot.runtime {
            RuntimeState::Running { .. } => {}
            RuntimeState::Starting => return Some(theme::INFO),
            RuntimeState::Closing { .. } => return Some(MUTED),
            RuntimeState::Detached => {}
        }
    }
    if state
        .session_ui
        .get(&info.id)
        .is_some_and(|ui| ui.working())
    {
        return (state.selected != Some(info.id)).then_some(theme::INFO);
    }
    match state.session_statuses.get(&info.id) {
        Some(SessionStatus::NeedsAttention) => Some(theme::WARNING),
        Some(SessionStatus::Recoverable) => Some(theme::INFO),
        _ => None,
    }
}

pub(super) fn render(frame: &mut Frame<'_>, plan: &LayoutPlan, hits: &mut HitMap, state: &UiState) {
    let Some(area) = plan.session_rail else {
        return;
    };
    hits.push(HitRegion {
        area,
        target: HitTarget::SessionRail,
    });
    frame.render_widget(Block::default().style(theme::surface(RAIL)), area);
    let active = focus::workspace_focused(state, Focus::Sessions);
    let project = state
        .workspace
        .as_ref()
        .map(|(_, name)| single_line_external(name))
        .unwrap_or_else(|| "BONE".into());
    let header = Rect::new(area.x + 3, area.y + 1, area.width.saturating_sub(6), 1);
    let header_background = if active && state.sessions.iter().all(|session| session.archived) {
        SELECTED
    } else {
        RAIL
    };
    frame.render_widget(
        Block::default().style(theme::surface(header_background)),
        header,
    );
    frame.render_widget(
        Paragraph::new(project).style(theme::label_on(INK, header_background)),
        header,
    );

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as i64);
    for row in &plan.session_rows {
        render_session_row(frame, plan, state, row.index, row.area, active, now);
        if let Some(session) = state.sessions.get(row.index) {
            hits.push(HitRegion {
                area: row.area,
                target: HitTarget::Session(session.id),
            });
        }
    }

    if active {
        frame.render_widget(
            Paragraph::new("↑↓ browse · enter open").style(theme::body(MUTED)),
            Rect::new(
                area.x + 2,
                area.bottom().saturating_sub(1),
                area.width.saturating_sub(4),
                1,
            ),
        );
    }
}

fn render_session_row(
    frame: &mut Frame<'_>,
    plan: &LayoutPlan,
    state: &UiState,
    index: usize,
    area: Rect,
    rail_active: bool,
    now: i64,
) {
    let info = &state.sessions[index];
    let current = state.selected == Some(info.id);
    let candidate = rail_active && state.session_candidate.or(state.selected) == Some(info.id);
    let background = if candidate { SELECTED } else { RAIL };
    frame.render_widget(Block::default().style(theme::surface(background)), area);

    let summary = state.session_summaries.get(&info.id);
    let pending = summary.is_some_and(|summary| summary.projection_pending);
    let preview = if pending {
        "Loading history…".into()
    } else {
        summary
            .and_then(|summary| summary.latest_reply_preview.as_deref())
            .map(single_line_external)
            .filter(|preview| !preview.trim().is_empty())
            .unwrap_or_else(|| "No replies yet".into())
    };
    let created = summary.map_or(0, |summary| summary.created_at);
    let count = summary.map_or_else(
        || "0 msgs".to_owned(),
        |summary| {
            if pending {
                "… msgs".to_owned()
            } else {
                format!("{} msgs", summary.message_count)
            }
        },
    );
    let meta = format!("{count}  ·  {}", format_created_at(created, now));
    let content = Rect::new(
        area.x + 2,
        area.y,
        area.width.saturating_sub(3),
        area.height.min(3),
    );
    if content.height > 0 {
        frame.render_widget(
            Paragraph::new(single_line_external(&info.title))
                .style(theme::label_on(INK, background)),
            Rect::new(content.x, content.y, content.width, 1),
        );
    }
    if content.height > 1 {
        frame.render_widget(
            Paragraph::new(preview).style(theme::body_on(MUTED, background)),
            Rect::new(content.x, content.y + 1, content.width, 1),
        );
    }
    if content.height > 2 {
        frame.render_widget(
            Paragraph::new(meta).style(theme::body_on(MUTED, background)),
            Rect::new(content.x, content.y + 2, content.width, 1),
        );
    }

    if current && area.width > 0 {
        for y in area.y..area.bottom().min(area.y + 3) {
            frame.buffer_mut()[(area.x, y)]
                .set_symbol(" ")
                .set_style(theme::surface(theme::FOCUS_MARK));
        }
    }
    let draft = state.session_ui.get(&info.id).map_or_else(
        || {
            summary.is_some_and(|summary| summary.has_draft)
                || matches!(
                    state.session_statuses.get(&info.id),
                    Some(SessionStatus::Draft)
                )
        },
        |ui| !ui.draft.is_empty(),
    );
    let tone = status_tone(state, index).or(draft.then_some(MUTED));
    if let Some(tone) = tone
        && area.width > 1
        && area.height > 0
    {
        frame.buffer_mut()[(area.x + 1, area.y)]
            .set_symbol("•")
            .set_style(theme::body_on(tone, background));
    }

    let divider_y = area.bottom();
    if divider_y
        < plan
            .session_rail
            .map_or(plan.screen.bottom(), |rail| rail.bottom() - 1)
    {
        for x in area.x..area.right() {
            frame.buffer_mut()[(x, divider_y)]
                .set_symbol("─")
                .set_style(theme::body_on(theme::STRUCTURE, RAIL));
        }
    }
}

fn format_created_at(created_at: i64, now: i64) -> String {
    if created_at <= 0 {
        return "—".into();
    }
    let elapsed = now.saturating_sub(created_at).max(0) / 1_000;
    if elapsed < 60 {
        return "now".into();
    }
    if elapsed < 60 * 60 {
        return format!("{}m", elapsed / 60);
    }
    if elapsed < 24 * 60 * 60 {
        return format!("{}h", elapsed / (60 * 60));
    }
    if elapsed < 7 * 24 * 60 * 60 {
        return format!("{}d", elapsed / (24 * 60 * 60));
    }
    let (year, month, day) = civil_date(created_at / 86_400_000);
    format!("{year:04}-{month:02}-{day:02}")
}

// Howard Hinnant's proleptic-Gregorian civil-from-days conversion. Keeping it
// here avoids a date-time dependency for a display-only UTC calendar label.
fn civil_date(days_since_epoch: i64) -> (i64, i64, i64) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month, day)
}

pub(super) fn problem_status(problem: &bone_app::AppProblem) -> (&'static str, Color) {
    use bone_app::AppProblem::*;
    match problem {
        Configuration(_) => ("Needs configuration", theme::WARNING),
        LoginRequired(_) => ("Needs login", theme::WARNING),
        ProfileBusy(_) => ("Profile busy", theme::WARNING),
        Provider(_) => ("Provider error", DANGER),
        Storage(_) => ("Storage error", DANGER),
        Tools(_) => ("Tool error", DANGER),
        Agent(_) => ("Agent error", DANGER),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{layout::SinglePane, state::Focus};
    use bone_app::{SessionId, SessionInfo, SessionSeq, SessionSummary, WorkspaceId};
    use ratatui::{Terminal, backend::TestBackend};

    fn fixture() -> (UiState, LayoutPlan) {
        let workspace = WorkspaceId::new();
        let current = SessionInfo {
            id: SessionId::new(),
            workspace,
            title: "Current".into(),
            archived: false,
        };
        let candidate = SessionInfo {
            id: SessionId::new(),
            workspace,
            title: "Candidate".into(),
            archived: false,
        };
        let mut state = UiState::default();
        state.sessions = vec![current.clone(), candidate.clone()];
        state.session_summaries = [
            SessionSummary {
                session: current.clone(),
                created_at: 1_725_523_200_000,
                message_count: 12,
                latest_reply_preview: Some("Latest assistant reply".into()),
                projection_pending: false,
                has_draft: false,
                draft_bytes: 0,
                persisted_runtime: None,
                history_through: SessionSeq(12),
            },
            SessionSummary {
                session: candidate.clone(),
                created_at: 1_725_523_500_000,
                message_count: 3,
                latest_reply_preview: Some("Candidate reply".into()),
                projection_pending: false,
                has_draft: false,
                draft_bytes: 0,
                persisted_runtime: None,
                history_through: SessionSeq(3),
            },
        ]
        .into_iter()
        .map(|summary| (summary.session.id, summary))
        .collect();
        state.selected = Some(current.id);
        state.session_candidate = Some(candidate.id);
        state.focus = Focus::Sessions;
        state.workspace = Some((workspace, "Workspace".into()));
        let plan = LayoutPlan::calculate(
            Rect::new(0, 0, 120, 24),
            SinglePane::Conversation,
            0,
            &[4, 4],
            Some(1),
            2,
        );
        (state, plan)
    }

    #[test]
    fn rows_are_three_lines_with_dividers_and_distinct_identity_and_selection() {
        let (state, plan) = fixture();
        let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
        terminal
            .draw(|frame| render(frame, &plan, &mut HitMap::default(), &state))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let current = plan.session_rows.iter().find(|row| row.index == 0).unwrap();
        let candidate = plan.session_rows.iter().find(|row| row.index == 1).unwrap();

        for y in current.area.y..current.area.y + 3 {
            assert_eq!(buffer[(current.area.x, y)].bg, theme::FOCUS_MARK);
        }
        for y in candidate.area.y..candidate.area.y + 3 {
            assert_eq!(buffer[(candidate.area.x + 2, y)].bg, SELECTED);
        }
        assert_ne!(SELECTED, theme::FOCUS_MARK);
        assert_eq!(
            buffer[(current.area.x, current.area.bottom())].symbol(),
            "─"
        );
        let current_text: String = (current.area.y..current.area.bottom())
            .flat_map(|y| (current.area.x..current.area.right()).map(move |x| (x, y)))
            .map(|position| buffer[position].symbol())
            .collect();
        assert!(current_text.contains("Current"));
        assert!(current_text.contains("Latest assistant reply"));
        assert!(current_text.contains("12 msgs"));
    }

    #[test]
    fn age_uses_relative_units_then_a_stable_calendar_date() {
        let now = 1_725_523_800_000;
        assert_eq!(format_created_at(now - 5 * 60_000, now), "5m");
        assert_eq!(format_created_at(now - 2 * 3_600_000, now), "2h");
        assert_eq!(format_created_at(now - 3 * 86_400_000, now), "3d");
        assert_eq!(format_created_at(0, now), "—");
        assert_eq!(civil_date(0), (1970, 1, 1));
    }

    #[test]
    fn warnings_and_failures_do_not_reuse_identity_or_caret_orange() {
        for problem in [
            bone_app::AppProblem::Configuration(bone_app::ConfigProblem::NeedsModel),
            bone_app::AppProblem::LoginRequired(bone_app::ProfileId::new("chatgpt").unwrap()),
            bone_app::AppProblem::Provider("provider".into()),
        ] {
            assert_ne!(problem_status(&problem).1, theme::FOCUS_MARK);
        }
    }

    #[test]
    fn pending_projection_never_presents_partial_history_as_complete() {
        let (mut state, plan) = fixture();
        let candidate = state.sessions[1].id;
        state
            .session_summaries
            .get_mut(&candidate)
            .unwrap()
            .projection_pending = true;
        let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
        terminal
            .draw(|frame| render(frame, &plan, &mut HitMap::default(), &state))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let row = plan.session_rows.iter().find(|row| row.index == 1).unwrap();
        let text: String = (row.area.y..row.area.bottom())
            .flat_map(|y| (row.area.x..row.area.right()).map(move |x| (x, y)))
            .map(|position| buffer[position].symbol())
            .collect();

        assert!(text.contains("Loading history…"));
        assert!(text.contains("… msgs"));
        assert!(!text.contains("Candidate reply"));
        assert!(!text.contains("3 msgs"));
    }

    #[test]
    fn an_empty_focused_rail_uses_its_header_without_a_new_session_button() {
        let mut state = UiState::default();
        state.focus = Focus::Sessions;
        state.workspace = Some((WorkspaceId::new(), "Workspace".into()));
        let plan = LayoutPlan::calculate(
            Rect::new(0, 0, 120, 24),
            SinglePane::Conversation,
            0,
            &[],
            None,
            1,
        );
        let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
        terminal
            .draw(|frame| render(frame, &plan, &mut HitMap::default(), &state))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let header = (plan.session_rail.unwrap().x + 3, 1);
        let text: String = buffer.content().iter().map(|cell| cell.symbol()).collect();

        assert_eq!(buffer[header].bg, SELECTED);
        assert_ne!(buffer[header].bg, theme::FOCUS_MARK);
        assert!(!text.contains("New session"));
    }
}
