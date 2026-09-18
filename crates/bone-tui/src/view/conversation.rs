use bone_app::{ActivityKind, JobState, RuntimeState};
use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    text::Line,
    widgets::{Block, Padding, Paragraph, Wrap},
};

use crate::{
    layout::{ClickRegion, ClickTarget, LayoutMode, LayoutPlan, TranscriptMetrics},
    state::{Action, UiState, WorkspaceTarget},
    ui::{
        focus,
        interaction::SurfaceHits,
        selection::{CopySource, SelectableText, TextRow},
        theme,
    },
    view::{composer, message, single_line_external},
};

pub(super) struct RenderedConversation {
    pub(super) caret: Option<(u16, u16)>,
    pub(super) metrics: Option<TranscriptMetrics>,
    pub(super) composer_row_origin: Option<usize>,
    pub(super) title_byte_origin: Option<usize>,
}

pub(super) fn render(
    frame: &mut Frame<'_>,
    plan: &LayoutPlan,
    hits: &mut SurfaceHits,
    state: &UiState,
    previous_composer_row: usize,
) -> RenderedConversation {
    if plan.mode == LayoutMode::TooSmall {
        render_too_small(frame, plan.conversation.unwrap_or(plan.screen), state);
        return RenderedConversation {
            caret: None,
            metrics: None,
            composer_row_origin: None,
            title_byte_origin: None,
        };
    }
    let (Some(header), Some(transcript), Some(composer_area)) =
        (plan.session_header, plan.transcript, plan.composer)
    else {
        return RenderedConversation {
            caret: None,
            metrics: None,
            composer_row_origin: None,
            title_byte_origin: None,
        };
    };
    hits.push(ClickRegion {
        area: header,
        target: ClickTarget::Editor(crate::state::EditorTarget::SessionTitle),
    });

    let (title_byte_origin, title_caret) = render_header(frame, header, state);
    if let (Some(session), Some(text)) = (state.selected, state.title_text()) {
        hits.push_text(SelectableText {
            source: CopySource::Title(session),
            item: 0,
            text: text.into(),
            rows: vec![TextRow::new(header, text, title_byte_origin..text.len())],
        });
    }
    if crate::layout::comfortable(plan.screen) {
        paint_rule(
            frame,
            Rect::new(transcript.x, header.y + 2, transcript.width, 1),
            theme::STRUCTURE,
        );
    }
    let metrics = if state.details.is_some() && plan.extension_blank.is_none() {
        None
    } else {
        hits.push_scroll(
            transcript,
            crate::ui::interaction::ScrollTarget::Conversation,
        );
        render_transcript(frame, transcript, state, hits)
    };
    let status = execution_status(state, composer_area.width.saturating_sub(4));
    let status_tone = status_tone(state);
    let status_area = Rect::new(
        composer_area.x + 2,
        composer_area
            .y
            .saturating_sub(if plan.screen.height < 18 { 1 } else { 2 }),
        composer_area.width.saturating_sub(4),
        1,
    );
    {
        frame.render_widget(
            Paragraph::new(single_line_external(&status)).style(theme::body(status_tone)),
            status_area,
        );
    }
    if let Some(ui) = state.selected_ui()
        && let Some(answer) = ui.active_answer()
    {
        let active = ui.snapshot.as_ref().is_some_and(|snapshot| {
            crate::state::answer::active_question(snapshot, answer.question).is_some()
        });
        let (label, target) = if active {
            (
                "Answering · back to draft",
                ClickTarget::Action(Action::LeaveAnswer),
            )
        } else {
            (
                "Question ended · keep as draft",
                ClickTarget::Action(Action::ConvertAnswer),
            )
        };
        let label = if state.activity_animation_active() {
            format!("{status} · {label}")
        } else {
            label.to_owned()
        };
        frame.render_widget(
            Paragraph::new(label).style(theme::body_on(theme::WARNING, theme::PANEL)),
            status_area,
        );
        hits.push(ClickRegion {
            area: status_area,
            target,
        });
    }
    let (composer_row_origin, composer_caret) = composer::render(
        frame,
        plan.screen,
        composer_area,
        hits,
        state,
        previous_composer_row,
    );
    RenderedConversation {
        caret: composer_caret.or(title_caret),
        metrics,
        composer_row_origin: Some(composer_row_origin),
        title_byte_origin: Some(title_byte_origin),
    }
}

fn problem_hint(problem: &bone_app::AppProblem, width: u16) -> &'static str {
    match problem {
        bone_app::AppProblem::Configuration(_) if width >= 28 => "Needs configuration · /model",
        bone_app::AppProblem::Configuration(_) => "Configure: /model",
        bone_app::AppProblem::LoginRequired(_) if width >= 20 => "Needs login · /model",
        bone_app::AppProblem::LoginRequired(_) => "Login: /model",
        bone_app::AppProblem::Credential(problem)
            if problem.kind == bone_app::CredentialProblemKind::Missing && width >= 24 =>
        {
            "Needs API key · /model"
        }
        bone_app::AppProblem::Credential(problem)
            if problem.kind == bone_app::CredentialProblemKind::Missing =>
        {
            "API key: /model"
        }
        _ => super::session_rail::problem_status(problem).0,
    }
}

fn status_tone(state: &UiState) -> ratatui::style::Color {
    if state.activity_animation_active() {
        return theme::INFO;
    }
    if state.status.is_some() {
        return theme::MUTED;
    }
    let Some(ui) = state.selected_ui() else {
        return theme::MUTED;
    };
    let Some(snapshot) = &ui.snapshot else {
        return theme::MUTED;
    };
    if let Some(problem) = &snapshot.problem {
        super::session_rail::problem_status(problem).1
    } else if ui.transcript.reading()
        || matches!(
            snapshot.runtime,
            RuntimeState::Starting | RuntimeState::Running { .. }
        )
    {
        theme::INFO
    } else {
        theme::MUTED
    }
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
        .style(theme::body(theme::WARNING))
        .wrap(Wrap { trim: false }),
        area,
    );
}

fn render_header(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &UiState,
) -> (usize, Option<(u16, u16)>) {
    let focused =
        focus::workspace_focused(state, WorkspaceTarget::SessionTitle) && state.selected.is_some();
    let fallback = state.title_text().unwrap_or("New conversation");
    let (title, cursor, selection_cells, byte_origin) = if focused {
        state.title_editor().map_or_else(
            || (single_line_external(fallback), 0, Vec::new(), 0),
            |editor| {
                let viewport = crate::editor::single_line_editor_viewport(
                    editor.text(),
                    editor.cursor(),
                    area.width,
                );
                let selection_cells = editor.selection().map_or_else(Vec::new, |selection| {
                    crate::editor::single_line_selection_cells(
                        editor.text(),
                        viewport.byte_origin,
                        area.width,
                        selection,
                    )
                });
                (
                    viewport.text,
                    viewport.cursor_x,
                    selection_cells,
                    viewport.byte_origin,
                )
            },
        )
    } else {
        (single_line_external(fallback), 0, Vec::new(), 0)
    };
    frame.render_widget(Block::default().style(theme::surface(theme::PANEL)), area);
    frame.render_widget(
        Paragraph::new(title).style(theme::label_on(theme::INK, theme::PANEL)),
        area,
    );
    for (x, width) in selection_cells {
        for dx in 0..width {
            frame.buffer_mut()[(area.x + x + dx, area.y)]
                .set_bg(theme::SELECTED)
                .set_fg(theme::INK);
        }
    }
    let caret = (focused && area.width > 0)
        .then_some((area.x + cursor.min(area.width.saturating_sub(1)), area.y));
    (byte_origin, caret)
}

fn paint_rule(frame: &mut Frame<'_>, area: Rect, tone: ratatui::style::Color) {
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            frame.buffer_mut()[(x, y)]
                .set_symbol("─")
                .set_style(theme::body_on(tone, theme::PANEL));
        }
    }
}

fn reader_selects(state: &UiState, source: crate::state::reader::ReaderSource) -> bool {
    matches!(&state.details, Some(reader)
        if Some(reader.content.session) == state.selected && reader.content.source == source)
}

fn render_transcript(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &UiState,
    hits: &mut SurfaceHits,
) -> Option<TranscriptMetrics> {
    let Some(session) = state.selected_ui() else {
        frame.render_widget(
            Paragraph::new("Start with a clear request.")
                .style(Style::default().fg(theme::MUTED))
                .block(Block::default().padding(Padding::new(2, 2, 1, 0))),
            area,
        );
        return None;
    };

    let mut rows = Vec::<Line<'static>>::new();
    let mut links = Vec::new();
    let mut anchors = Vec::new();
    let mut copy_items = Vec::new();
    let mut previous_compact = false;
    for entry in session.transcript.entries() {
        let (mut rendered, text) =
            if let bone_app::SessionEvent::ToolFinished { outcome, .. } = &entry.event {
                // Share the existing bounded source cache with tool summaries. Animation
                // frames must not reinterpret results or create another copy of them.
                let text = session
                    .transcript
                    .copy_text(entry.sequence, || message::copy_text(&entry.event, &[]));
                let label = text
                    .split_once('\n')
                    .map_or(text.as_ref(), |(label, _)| label);
                (message::render_tool(outcome, label, area.width), text)
            } else {
                let rendered = message::render(&entry.event, area.width);
                let text = session.transcript.copy_text(entry.sequence, || {
                    message::copy_text(&entry.event, &rendered)
                });
                (rendered, text)
            };
        if reader_selects(
            state,
            crate::state::reader::ReaderSource::History(entry.sequence),
        ) {
            for row in &mut rendered {
                row.line.style = row.line.style.bg(theme::SELECTED);
            }
        }
        let copy_rows: Vec<_> = rendered
            .iter()
            .enumerate()
            .map(|(index, row)| {
                message::copy_row(&entry.event, row, index, area.width).map(|(byte, gutter)| {
                    let width = if matches!(
                        &entry.event,
                        bone_app::SessionEvent::InputSubmitted { .. }
                            | bone_app::SessionEvent::Reply { .. }
                    ) {
                        area.width.saturating_sub(4).max(1)
                    } else {
                        let displayed = row.line.width().min(usize::from(area.width)) as u16;
                        let ellipsis = u16::from(
                            row.line
                                .spans
                                .last()
                                .is_some_and(|span| span.content.ends_with('…')),
                        );
                        displayed.saturating_sub(gutter).saturating_sub(ellipsis)
                    };
                    (byte, gutter, width)
                })
            })
            .collect();
        if rendered.is_empty() {
            copy_items.push((rows.len(), text, entry.sequence.0, copy_rows));
            continue;
        }
        let compact = compact_event(&entry.event);
        if !(rows.is_empty() || previous_compact && compact) {
            rows.push(Line::default());
            anchors.push(crate::layout::ContentAnchor {
                sequence: entry.sequence,
                byte: 0,
                part: crate::layout::AnchorPart::Separator,
            });
        }
        previous_compact = compact;
        let source_row = rows.len();
        copy_items.push((source_row, text, entry.sequence.0, copy_rows));
        let target = match entry.event {
            bone_app::SessionEvent::ToolFinished { .. }
            | bone_app::SessionEvent::ConversationFailed { .. }
            | bone_app::SessionEvent::InputRejected { .. } => {
                Some(ClickTarget::Action(Action::OpenHistory(entry.sequence)))
            }
            bone_app::SessionEvent::QuestionAsked { question, .. }
                if session.snapshot.as_ref().is_some_and(|snapshot| {
                    crate::state::answer::active_question(snapshot, question).is_some()
                }) =>
            {
                Some(ClickTarget::Action(Action::AnswerQuestion(question)))
            }
            _ => None,
        };
        if let Some(target) = target {
            links.push((source_row, target));
        }
        anchors.extend(rendered.iter().map(|row| crate::layout::ContentAnchor {
            sequence: entry.sequence,
            part: row.part,
            byte: row.byte,
        }));
        rows.extend(rendered.into_iter().map(|row| row.line));
    }

    if session.transcript.at_tail()
        && let Some(snapshot) = &session.snapshot
    {
        let mut ephemeral = Vec::new();
        let limit = usize::from(area.height);
        for job in snapshot.jobs.iter().rev() {
            if ephemeral.len() >= limit {
                break;
            }
            if matches!(job.state, JobState::Running | JobState::Waiting(_)) {
                let mut line =
                    message::compact("›", &format!("{}  [details]", job.goal), theme::INFO);
                if reader_selects(state, crate::state::reader::ReaderSource::Job(job.id)) {
                    line.style = line.style.bg(theme::SELECTED);
                }
                ephemeral.push((line, Some(ClickTarget::Action(Action::OpenJob(job.id)))));
            }
        }
        for activity in snapshot.activity.iter().rev() {
            if ephemeral.len() >= limit {
                break;
            }
            let name = match &activity.kind {
                ActivityKind::Converse => "Responding".to_owned(),
                ActivityKind::Work => "Working".to_owned(),
                ActivityKind::Compact => "Compacting context".to_owned(),
                ActivityKind::Tool { name, arguments } => {
                    let summary = bone_app::tool_summary(name, arguments, None);
                    let mut subject = if summary.subject == *name {
                        name.to_owned()
                    } else {
                        format!("{name} · {}", summary.subject)
                    };
                    if let Some(result) = summary.result {
                        subject.push_str(" · ");
                        subject.push_str(&result);
                    }
                    subject
                }
            };
            let body = activity.progress.as_deref().map_or_else(
                || single_line_external(&name),
                |progress| {
                    format!(
                        "{}  {}",
                        single_line_external(&name),
                        single_line_external(progress)
                    )
                },
            );
            ephemeral.push((message::compact("·", &body, theme::MUTED), None));
        }
        ephemeral.reverse();
        if !ephemeral.is_empty() && !rows.is_empty() {
            rows.push(Line::default());
        }
        for (line, target) in ephemeral {
            if let Some(target) = target {
                links.push((rows.len(), target));
            }
            rows.push(line);
        }
    }

    // Snapshot-only actions also exist before history loads. They belong to the
    // live tail and must not be inserted into the user's older reading window.
    if session.transcript.at_tail() {
        if let Some(snapshot) = &session.snapshot {
            if let Some(question) = crate::state::answer::current_question(snapshot) {
                let in_history = session.transcript.entries().any(|entry| {
                    matches!(entry.event,
                    bone_app::SessionEvent::QuestionAsked { question: id, .. } if id == question.id)
                });
                if !in_history {
                    links.push((
                        rows.len(),
                        ClickTarget::Action(Action::AnswerQuestion(question.id)),
                    ));
                    rows.extend(message::body_rows(
                        question.text,
                        usize::from(area.width),
                        theme::WARNING,
                    ));
                }
            }
            for candidate in
                crate::state::answer::recoverable_inputs(snapshot, session.transcript.entries())
            {
                let (target, label) = match candidate {
                    crate::state::answer::RecoveryCandidate::Retry { input } => (
                        ClickTarget::Action(Action::RetryInput(input)),
                        format!("Retry saved input #{}", input.0),
                    ),
                    crate::state::answer::RecoveryCandidate::Restore { input, .. } => (
                        ClickTarget::Action(Action::RestoreInput(input)),
                        format!("Restore input #{} to draft", input.0),
                    ),
                };
                links.push((rows.len(), target));
                rows.push(message::compact("↳", &label, theme::INFO));
            }
        }
        if session
            .submitting
            .as_ref()
            .is_some_and(|pending| pending.failed)
        {
            links.push((rows.len(), ClickTarget::Action(Action::RetrySubmission)));
            rows.push(message::compact(
                "↳",
                "Retry original submission",
                theme::DANGER,
            ));
        }
    }
    if rows.is_empty() {
        frame.render_widget(
            Paragraph::new("  Start with a clear request.")
                .style(Style::default().fg(theme::MUTED)),
            area,
        );
        return Some(TranscriptMetrics {
            total_rows: 0,
            viewport_rows: usize::from(area.height),
            ..TranscriptMetrics::default()
        });
    }
    let viewport = usize::from(area.height);
    let start = session
        .transcript
        .read_anchor()
        .and_then(|anchor| {
            anchors
                .iter()
                .enumerate()
                .filter(|(_, candidate)| {
                    candidate.sequence == anchor.sequence
                        && candidate.part == anchor.part
                        && candidate.byte <= anchor.byte
                })
                .map(|(row, _)| row)
                .next_back()
                .or_else(|| {
                    anchors
                        .iter()
                        .position(|candidate| candidate.sequence >= anchor.sequence)
                })
        })
        .unwrap_or_else(|| {
            rows.len()
                .saturating_sub(session.transcript.scroll_from_tail())
                .saturating_sub(viewport)
        });
    let start = start.min(if session.transcript.read_anchor().is_some() {
        rows.len().saturating_sub(1)
    } else {
        rows.len().saturating_sub(viewport)
    });
    let end = (start + viewport).min(rows.len());
    let visible = rows[start..end].to_vec();
    for (origin, text, item, copy_rows) in copy_items {
        let mapped = copy_rows
            .into_iter()
            .enumerate()
            .filter_map(|(index, copy)| {
                let global = origin + index;
                let (byte, gutter, width) = copy?;
                if global < start || global >= end || gutter >= area.width {
                    return None;
                }
                let row_area = Rect::new(
                    area.x + gutter,
                    area.y + (global - start) as u16,
                    width.min(area.width - gutter),
                    1,
                );
                Some(TextRow::new(row_area, &text, byte..text.len()))
            })
            .collect();
        hits.push_text(SelectableText {
            source: CopySource::Transcript(state.selected.unwrap()),
            item,
            text,
            rows: mapped,
        });
    }
    for (row, target) in links {
        if row >= start && row < end {
            hits.push(ClickRegion {
                area: Rect::new(area.x, area.y + (row - start) as u16, area.width, 1),
                target,
            });
        }
    }
    frame.render_widget(Paragraph::new(visible), area);
    Some(TranscriptMetrics {
        total_rows: rows.len(),
        viewport_rows: viewport,
        anchors: anchors.into(),
        start_row: start,
        scroll_from_tail: rows.len().saturating_sub(end),
    })
}

#[cfg(test)]
fn visible_rows(
    rows: &[Line<'static>],
    viewport: usize,
    scroll_from_tail: usize,
) -> Vec<Line<'static>> {
    let end = rows.len().saturating_sub(scroll_from_tail.min(rows.len()));
    rows[end.saturating_sub(viewport)..end].to_vec()
}

fn execution_status(state: &UiState, width: u16) -> String {
    if let Some(problem) = state
        .selected_ui()
        .and_then(|ui| ui.snapshot.as_ref())
        .and_then(|snapshot| snapshot.problem.as_ref())
    {
        return problem_hint(problem, width).into();
    }
    let reading = state
        .selected_ui()
        .is_some_and(|ui| ui.transcript.reading());
    if let Some((label, active)) = state.execution_feedback() {
        const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        let mut text = if active {
            format!("{} {}", SPINNER[state.activity_frame], label)
        } else {
            label.to_owned()
        };
        if reading {
            text.push_str(" · Reading history · PgDn for latest");
        }
        // Transient notices may explain a failed action; they never hide active work.
        if !active && let Some(notice) = state.status_text() {
            return notice.to_owned();
        }
        return text;
    }
    state.status_text().map(str::to_owned).unwrap_or_else(|| {
        if reading {
            "Reading history · PgDn for latest".into()
        } else {
            String::new()
        }
    })
}

fn compact_event(event: &bone_app::SessionEvent) -> bool {
    use bone_app::SessionEvent;
    match event {
        SessionEvent::ToolFinished { outcome, .. } => outcome.result.is_ok(),
        SessionEvent::InputFinished { outcome, .. } => *outcome != bone_app::InputOutcome::Failed,
        SessionEvent::InputCancelled { .. }
        | SessionEvent::AcceptanceRecorded { .. }
        | SessionEvent::WriteResolved { .. } => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::{
        layout::{ClickRegion, ClickTarget},
        state::{PendingSubmission, SessionUi},
    };
    use bone_app::{
        HistoryEntry, HistoryPage, InputId, InputState, InputView, JobOwner, JobRef, JobView,
        ModelSelection, Profile, ProfileId, QuestionId, RecentHistoryPage, RequestId,
        ResolvedModel, RuntimeConfig, RuntimeId, SessionEvent, SessionId, SessionInfo, SessionSeq,
        SessionView, WorkspaceId,
    };
    use ratatui::{Terminal, backend::TestBackend};
    use std::sync::Arc;

    #[test]
    fn feedback_tracks_work_and_stops_for_waiting_failure_and_completion() {
        let (mut state, q) = fixture();
        assert_eq!(state.execution_feedback(), None, "idle runtime is not work");
        for (input_state, expected, animated) in [
            (
                InputState::Posting { runtime: q.runtime },
                "Preparing next step",
                true,
            ),
            (
                InputState::Accepted { runtime: q.runtime },
                "Preparing next step",
                true,
            ),
            (active_input(q).state, "Waiting for your answer", false),
            (
                InputState::ConversationFailed {
                    runtime: q.runtime,
                    message: "denied".into(),
                },
                "Request failed · open error for details",
                false,
            ),
            (
                InputState::Finished {
                    runtime: q.runtime,
                    outcome: bone_app::InputOutcome::Completed,
                },
                "Completed",
                false,
            ),
        ] {
            let snapshot =
                Arc::make_mut(state.selected_ui_mut().unwrap().snapshot.as_mut().unwrap());
            let mut input = active_input(q);
            input.state = input_state;
            snapshot.inputs = vec![input];
            assert_eq!(state.execution_feedback(), Some((expected, animated)));
            assert_eq!(state.activity_animation_active(), animated);
        }
    }

    #[test]
    fn a_blocked_worker_does_not_invent_a_user_question() {
        let (mut state, q) = fixture();
        let snapshot = Arc::make_mut(state.selected_ui_mut().unwrap().snapshot.as_mut().unwrap());
        snapshot.inputs.clear();
        snapshot.jobs.push(JobView {
            id: JobRef {
                runtime: q.runtime,
                id: 1,
            },
            owner: JobOwner::User,
            inputs: vec![],
            goal: "blocked work".into(),
            scope: "scope".into(),
            done_when: "done".into(),
            allowed_tools: ["read".into()].into(),
            state: JobState::Waiting(bone_app::WaitReason::User),
            report: None,
        });
        assert!(crate::state::answer::current_question(snapshot).is_none());
        assert_ne!(
            state.execution_feedback(),
            Some(("Waiting for your answer", false))
        );
        let snapshot = Arc::make_mut(state.selected_ui_mut().unwrap().snapshot.as_mut().unwrap());
        snapshot.activity.push(bone_app::ActivityView {
            call: bone_app::CallRef {
                runtime: q.runtime,
                id: 2,
            },
            job: None,
            kind: ActivityKind::Converse,
            progress: None,
        });
        assert_eq!(state.execution_feedback(), Some(("Responding", true)));
    }

    #[test]
    fn activity_ticks_keep_reading_draft_focus_and_details_unchanged() {
        use crate::state::{UiEvent, update};
        let (mut state, q) = fixture();
        let ui = state.selected_ui_mut().unwrap();
        ui.draft = "中文草稿".into();
        ui.transcript.scroll_up(5);
        let snapshot = Arc::make_mut(ui.snapshot.as_mut().unwrap());
        let mut input = active_input(q);
        input.state = InputState::Accepted { runtime: q.runtime };
        snapshot.inputs.push(input);
        state.details = Some(crate::state::ReaderState {
            content: crate::state::reader::ReaderContent::from_history(
                state.selected.unwrap(),
                &HistoryEntry {
                    sequence: SessionSeq(1),
                    occurred_at: 0,
                    event: SessionEvent::ConversationFailed {
                        runtime: q.runtime,
                        inputs: vec![],
                        message: "older failure".into(),
                    },
                },
            )
            .unwrap(),
            scroll: 3,
        });
        state.status = Some(crate::state::Status::from("old notice"));
        let before = execution_status(&state, 100);
        assert!(before.contains("Preparing next step"));
        assert!(before.contains("Reading history"));
        let keyboard = state.keyboard;
        assert!(update(&mut state, UiEvent::ActivityTick).is_empty());
        assert_ne!(execution_status(&state, 100), before);
        assert_eq!(state.keyboard, keyboard);
        assert_eq!(state.selected_ui().unwrap().draft(), "中文草稿");
        assert!(state.selected_ui().unwrap().transcript.reading());
        assert_eq!(state.details.as_ref().unwrap().scroll, 3);
        Arc::make_mut(state.selected_ui_mut().unwrap().snapshot.as_mut().unwrap())
            .inputs
            .clear();
        state.dirty = false;
        let frame = state.activity_frame;
        update(&mut state, UiEvent::ActivityTick);
        assert_eq!(state.activity_frame, frame);
        assert!(!state.dirty);
    }

    #[test]
    fn running_tool_shows_its_object_and_live_progress() {
        let (mut state, q) = fixture();
        let snapshot = Arc::make_mut(state.selected_ui_mut().unwrap().snapshot.as_mut().unwrap());
        snapshot.activity.push(bone_app::ActivityView {
            call: bone_app::CallRef {
                runtime: q.runtime,
                id: 1,
            },
            job: None,
            kind: ActivityKind::Tool {
                name: "read".into(),
                arguments: serde_json::json!({"path":"src/main.rs","offset":20}),
            },
            progress: Some("Reading contents".into()),
        });
        let (text, _, _) = render_rows(&state, 10);
        assert!(text.contains("read · src/main.rs · from line 20"));
        assert!(text.contains("Reading contents"));
    }

    #[test]
    fn compact_events_share_spacing_but_errors_keep_a_boundary() {
        let (mut state, q) = fixture();
        let events = vec![
            SessionEvent::InputAccepted {
                runtime: q.runtime,
                input: InputId(1),
            },
            SessionEvent::InputCancelled { input: InputId(1) },
            SessionEvent::ConversationFailed {
                runtime: q.runtime,
                inputs: vec![],
                message: "failure".into(),
            },
        ];
        replace_history(
            state.selected_ui_mut().unwrap(),
            events
                .into_iter()
                .enumerate()
                .map(|(index, event)| HistoryEntry {
                    sequence: SessionSeq(index as u64 + 1),
                    occurred_at: 0,
                    event,
                })
                .collect(),
        );
        let (text, _, metrics) = render_rows(&state, 12);
        let rows: Vec<_> = text.lines().collect();
        assert!(!text.contains("Request received"));
        assert!(rows[0].contains("Request cancelled"));
        assert!(rows[1].trim().is_empty());
        assert!(rows[2].contains("failure"));
        assert_eq!(metrics.total_rows, 3);
    }

    #[test]
    fn title_focus_uses_only_the_blinking_caret_and_keeps_the_rule_neutral() {
        let header = Rect::new(4, 1, 30, 1);
        let rule = Rect::new(2, 3, 32, 1);
        for (focus_state, panel, caret_visible, expected_orange_cells) in [
            (crate::state::WorkspaceTarget::SessionTitle, None, true, 1),
            (crate::state::WorkspaceTarget::SessionTitle, None, false, 0),
            (crate::state::WorkspaceTarget::Composer, None, true, 0),
            (
                crate::state::WorkspaceTarget::SessionTitle,
                Some(crate::state::Overlay::Help),
                true,
                0,
            ),
        ] {
            let (mut state, _) = fixture();
            state.set_workspace_target(focus_state);
            state.overlay = panel;
            if state.overlay.is_some() {
                state.enter_overlay();
            }
            state.caret_visible = caret_visible;
            assert!(state.begin_title_edit());
            let mut terminal = Terminal::new(TestBackend::new(40, 5)).unwrap();
            terminal
                .draw(|frame| {
                    let (_, caret) = render_header(frame, header, &state);
                    if let Some(caret) = caret {
                        crate::ui::caret::place(frame, caret, state.caret_visible);
                    }
                    paint_rule(frame, rule, theme::STRUCTURE);
                })
                .unwrap();
            let buffer = terminal.backend().buffer();
            for x in rule.x..rule.right() {
                assert_eq!(buffer[(x, rule.y)].symbol(), "─");
                assert_eq!(buffer[(x, rule.y)].fg, theme::STRUCTURE);
                assert_eq!(buffer[(x, rule.y)].bg, theme::PANEL);
            }
            assert_eq!(buffer[(header.x, header.y)].symbol(), "t");
            let orange_cells = buffer
                .content()
                .iter()
                .filter(|cell| cell.fg == theme::FOCUS_MARK || cell.bg == theme::FOCUS_MARK)
                .count();
            assert_eq!(orange_cells, expected_orange_cells);
        }
    }

    #[test]
    fn title_selection_uses_neutral_cells_in_the_scrolled_unicode_viewport() {
        let (mut state, _) = fixture();
        let title = "ab中e\u{301}🙂zTAIL";
        let selected_from = title.find('z').unwrap();
        let session = state.selected.unwrap();
        state
            .session_rows
            .iter_mut()
            .find(|row| row.id() == session)
            .unwrap()
            .summary
            .session
            .title = title.into();
        state.set_workspace_target(crate::state::WorkspaceTarget::SessionTitle);
        state.caret_visible = true;
        assert!(state.begin_title_edit());
        crate::state::update(
            &mut state,
            crate::state::UiEvent::Action(crate::state::Action::Edit {
                target: crate::state::EditorTarget::SessionTitle,
                command: crate::editor::EditCommand::Point {
                    byte: selected_from,
                    extend: false,
                },
            }),
        );
        crate::state::update(
            &mut state,
            crate::state::UiEvent::Action(crate::state::Action::Edit {
                target: crate::state::EditorTarget::SessionTitle,
                command: crate::editor::EditCommand::Point {
                    byte: title.len(),
                    extend: true,
                },
            }),
        );

        let header = Rect::new(2, 1, 8, 1);
        let mut terminal = Terminal::new(TestBackend::new(12, 3)).unwrap();
        terminal
            .draw(|frame| {
                let (_, caret) = render_header(frame, header, &state);
                if let Some(caret) = caret {
                    crate::ui::caret::place(frame, caret, state.caret_visible);
                }
            })
            .unwrap();
        let buffer = terminal.backend().buffer();

        assert_eq!(buffer[(header.x, header.y)].symbol(), "🙂");
        assert_eq!(buffer[(header.x, header.y)].bg, theme::PANEL);
        // Ratatui deliberately resets and skips the hidden continuation cell of
        // a wide grapheme. The leading cell's style paints both terminal columns.
        assert_eq!(buffer[(header.x + 1, header.y)].symbol(), " ");
        assert_eq!(
            buffer[(header.x + 1, header.y)].bg,
            ratatui::style::Color::Reset
        );
        for x in 2..7 {
            let cell = &buffer[(header.x + x, header.y)];
            assert_eq!(cell.bg, theme::SELECTED);
            assert_eq!(cell.fg, theme::INK);
        }
        assert_eq!(buffer[(header.x + 7, header.y)].bg, theme::FOCUS_MARK);
        assert_ne!(theme::SELECTED, theme::FOCUS_MARK);
        assert_eq!(
            buffer
                .content()
                .iter()
                .filter(|cell| cell.fg == theme::FOCUS_MARK || cell.bg == theme::FOCUS_MARK)
                .count(),
            1
        );
    }

    #[test]
    fn configuration_and_login_hints_keep_the_next_command_visible() {
        let configuration =
            bone_app::AppProblem::Configuration(bone_app::ConfigProblem::NeedsModel);
        let login = bone_app::AppProblem::LoginRequired(ProfileId::new("chatgpt").unwrap());
        for (problem, command) in [(configuration, "/model"), (login, "/model")] {
            for width in [18, 24, 36, 80] {
                let label = problem_hint(&problem, width);
                assert!(label.contains(command));
                assert!(unicode_width::UnicodeWidthStr::width(label) <= usize::from(width));
            }
        }
    }

    #[test]
    fn status_tone_uses_typed_state_and_keeps_untyped_text_neutral() {
        let (mut state, _) = fixture();
        assert_eq!(status_tone(&state), theme::INFO);

        state.status = Some("opaque external status".into());
        assert_eq!(status_tone(&state), theme::MUTED);
        state.status = None;

        Arc::make_mut(state.selected_ui_mut().unwrap().snapshot.as_mut().unwrap()).problem = Some(
            bone_app::AppProblem::Configuration(bone_app::ConfigProblem::NeedsModel),
        );
        assert_eq!(status_tone(&state), theme::WARNING);
        assert_ne!(status_tone(&state), theme::FOCUS_MARK);

        Arc::make_mut(state.selected_ui_mut().unwrap().snapshot.as_mut().unwrap()).problem =
            Some(bone_app::AppProblem::Provider("offline".into()));
        assert_eq!(status_tone(&state), theme::DANGER);
        assert_ne!(status_tone(&state), theme::FOCUS_MARK);
    }

    fn fixture() -> (UiState, QuestionId) {
        let info = SessionInfo {
            id: SessionId::new(),
            workspace: WorkspaceId::new(),
            title: "test".into(),
            archived: false,
        };
        let q = QuestionId {
            runtime: RuntimeId::new(),
            record: 1,
            reply_to: InputId(1),
        };
        let model = ResolvedModel {
            selection: ModelSelection::new(ProfileId::chatgpt(), "test").unwrap(),
            profile: Profile::chatgpt(),
        };
        let snapshot = SessionView {
            session: info.clone(),
            runtime: RuntimeState::Running {
                id: q.runtime,
                config: Box::new(RuntimeConfig {
                    coordinator: model.clone(),
                    worker: model,
                    limits: Default::default(),
                    tools: Default::default(),
                    workspace: Default::default(),
                }),
            },
            draft: String::new(),
            inputs: vec![],
            jobs: vec![],
            activity: vec![],
            history_through: SessionSeq(0),
            problem: None,
        };
        let mut ui = SessionUi::new(info.id, 1);
        ui.snapshot = Some(Arc::new(snapshot));
        let mut state = UiState::default();
        state.selected = Some(info.id);
        state
            .session_rows
            .push(crate::state::SessionNavRow::provisional(info.clone()));
        state.session_ui.insert(info.id, ui);
        (state, q)
    }

    fn active_input(q: QuestionId) -> InputView {
        InputView {
            id: q.reply_to,
            request_id: RequestId::new(),
            text: "request".into(),
            reply_to: None,
            state: InputState::WaitingForUser {
                runtime: q.runtime,
                question: q,
                text: "Which scope?".into(),
            },
        }
    }

    fn replace_history(ui: &mut SessionUi, items: Vec<HistoryEntry>) {
        let snapshot_through = items.last().map_or(SessionSeq(0), |entry| entry.sequence);
        ui.transcript.open(RecentHistoryPage {
            items,
            older_cursor: None,
            snapshot_through,
        });
    }

    fn append_history(ui: &mut SessionUi, items: Vec<HistoryEntry>) {
        let next_cursor = items.last().map_or(SessionSeq(0), |entry| entry.sequence);
        ui.transcript.history_loaded(HistoryPage {
            items,
            next_cursor,
            has_more: false,
        });
    }

    fn render_rows(state: &UiState, height: u16) -> (String, Vec<ClickRegion>, TranscriptMetrics) {
        render_width(state, 80, height)
    }

    fn render_width(
        state: &UiState,
        width: u16,
        height: u16,
    ) -> (String, Vec<ClickRegion>, TranscriptMetrics) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let mut hits = SurfaceHits::default();
        let mut metrics = None;
        terminal
            .draw(|frame| {
                metrics = render_transcript(frame, frame.area(), state, &mut hits);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let text = (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        (text, hits.regions().to_vec(), metrics.unwrap())
    }

    #[test]
    fn original_byte_anchor_survives_chinese_code_reflow_and_history_growth() {
        for kind in 0..3 {
            let (mut state, question) = fixture();
            let source = format!(
                "标题\n```rust\n    {}锚{}\n```\n结束",
                "中文变量".repeat(67),
                "连续中文代码".repeat(120)
            );
            let anchor = crate::layout::ContentAnchor {
                sequence: SessionSeq(20),
                byte: source.find('锚').unwrap(),
                part: crate::layout::AnchorPart::Text,
            };
            let event = match kind {
                0 => SessionEvent::InputSubmitted {
                    input: InputId(1),
                    request_id: RequestId::new(),
                    text: source,
                    reply_to: None,
                },
                1 => SessionEvent::Reply {
                    inputs: vec![],
                    text: source,
                },
                _ => SessionEvent::QuestionAsked {
                    question,
                    inputs: vec![],
                    text: source,
                },
            };
            let ui = state.selected_ui_mut().unwrap();
            replace_history(
                ui,
                vec![HistoryEntry {
                    sequence: SessionSeq(20),
                    occurred_at: 0,
                    event,
                }],
            );
            ui.transcript.begin_reading_at(anchor);
            for width in [160, 40, 120, 80, 40, 160] {
                let (rendered, _, metrics) = render_width(&state, width, 12);
                assert!(
                    rendered.lines().next().unwrap().contains('锚'),
                    "kind={kind} width={width}: {rendered}"
                );
                assert_eq!(
                    metrics.anchor_at_start(metrics.start_row).unwrap().sequence,
                    SessionSeq(20)
                );
                assert_eq!(
                    state.selected_ui().unwrap().transcript.read_anchor(),
                    Some(anchor)
                );
            }
            let ui = state.selected_ui_mut().unwrap();
            for (sequence, front) in [(10, true), (30, false)] {
                let entry = HistoryEntry {
                    sequence: SessionSeq(sequence),
                    occurred_at: 0,
                    event: SessionEvent::InputSubmitted {
                        input: InputId(sequence),
                        request_id: RequestId::new(),
                        text: "other history".repeat(100),
                        reply_to: None,
                    },
                };
                if front {
                    ui.transcript.older_history_loaded(RecentHistoryPage {
                        items: vec![entry],
                        older_cursor: None,
                        snapshot_through: SessionSeq(30),
                    });
                } else {
                    append_history(ui, vec![entry]);
                }
            }
            let (rendered, _, _) = render_width(&state, 40, 12);
            assert!(rendered.lines().next().unwrap().contains('锚'));
        }
    }

    #[test]
    fn detail_return_keeps_original_reading_position_until_explicit_tail() {
        use crate::state::{Action, UiEvent, update};
        let (mut state, question) = fixture();
        let session = state.selected.unwrap();
        let ui = state.selected_ui_mut().unwrap();
        ui.draft = "keep my draft".into();
        replace_history(
            ui,
            vec![
                HistoryEntry {
                    sequence: SessionSeq(1),
                    occurred_at: 0,
                    event: SessionEvent::Reply {
                        inputs: vec![],
                        text: "长中文与代码内容\n".repeat(50),
                    },
                },
                HistoryEntry {
                    sequence: SessionSeq(2),
                    occurred_at: 0,
                    event: SessionEvent::JobFinished {
                        job: JobRef {
                            runtime: question.runtime,
                            id: 1,
                        },
                        outcome: bone_app::OutcomeKind::Completed,
                        summary: "done".into(),
                        remaining: vec![],
                    },
                },
            ],
        );
        let (_, _, metrics) = render_width(&state, 80, 12);
        state
            .selected_ui_mut()
            .unwrap()
            .transcript
            .retain_metrics(Arc::new(metrics), usize::MAX);
        update(
            &mut state,
            UiEvent::Action(Action::OpenHistory(SessionSeq(2))),
        );
        let anchor = state
            .selected_ui()
            .unwrap()
            .transcript
            .read_anchor()
            .unwrap();
        let mut snapshot = (**state.selected_ui().unwrap().snapshot.as_ref().unwrap()).clone();
        snapshot.history_through = SessionSeq(3);
        update(
            &mut state,
            UiEvent::SessionChanged {
                session,
                generation: 1,
                snapshot: Arc::new(snapshot),
            },
        );
        update(
            &mut state,
            UiEvent::HistoryLoaded {
                session,
                generation: 1,
                page: bone_app::HistoryPage {
                    items: vec![HistoryEntry {
                        sequence: SessionSeq(3),
                        occurred_at: 0,
                        event: SessionEvent::InputSubmitted {
                            input: InputId(3),
                            request_id: RequestId::new(),
                            text: "new background input".into(),
                            reply_to: None,
                        },
                    }],
                    next_cursor: SessionSeq(3),
                    has_more: false,
                },
            },
        );
        update(&mut state, UiEvent::Action(Action::Escape));
        assert_eq!(
            state.selected_ui().unwrap().transcript.read_anchor(),
            Some(anchor)
        );
        assert_eq!(state.selected_ui().unwrap().draft(), "keep my draft");
        let (_, _, metrics) = render_width(&state, 40, 12);
        assert_eq!(metrics.anchor_at_start(metrics.start_row).unwrap(), anchor);
        state
            .selected_ui_mut()
            .unwrap()
            .transcript
            .retain_metrics(Arc::new(metrics), usize::MAX);
        let effects = update(&mut state, UiEvent::Action(Action::ScrollDown(usize::MAX)));
        assert!(
            state
                .selected_ui()
                .unwrap()
                .transcript
                .read_anchor()
                .is_none()
        );
        assert!(
            effects
                .iter()
                .any(|effect| matches!(effect, crate::state::Effect::ReloadRecentHistory { .. }))
        );
    }

    #[test]
    fn user_padding_and_first_text_row_have_distinct_stable_anchors() {
        let (mut state, _) = fixture();
        replace_history(
            state.selected_ui_mut().unwrap(),
            vec![HistoryEntry {
                sequence: SessionSeq(1),
                occurred_at: 0,
                event: SessionEvent::InputSubmitted {
                    input: InputId(1),
                    request_id: RequestId::new(),
                    text: "first body line\nrest".into(),
                    reply_to: None,
                },
            }],
        );
        for part in [
            crate::layout::AnchorPart::UserTop,
            crate::layout::AnchorPart::Text,
        ] {
            let anchor = crate::layout::ContentAnchor {
                sequence: SessionSeq(1),
                byte: 0,
                part,
            };
            state
                .selected_ui_mut()
                .unwrap()
                .transcript
                .begin_reading_at(anchor);
            for width in [40, 120, 80] {
                let (rendered, _, metrics) = render_width(&state, width, 4);
                assert_eq!(metrics.anchor_at_start(metrics.start_row).unwrap(), anchor);
                assert_eq!(
                    rendered.lines().next().unwrap().contains("first body line"),
                    part == crate::layout::AnchorPart::Text
                );
            }
        }
    }

    #[test]
    fn active_question_is_visible_and_clickable_before_history_loads() {
        let (mut state, q) = fixture();
        Arc::make_mut(state.selected_ui_mut().unwrap().snapshot.as_mut().unwrap())
            .inputs
            .push(active_input(q));
        let (text, hits, metrics) = render_rows(&state, 8);
        assert_eq!(text.matches("Which scope?").count(), 1);
        assert!(!text.contains("Start with a clear request"));
        assert!(
            hits.iter()
                .any(|h| h.target == ClickTarget::Action(Action::AnswerQuestion(q)))
        );
        assert!(metrics.total_rows > 0);
        replace_history(
            state.selected_ui_mut().unwrap(),
            vec![HistoryEntry {
                sequence: SessionSeq(1),
                occurred_at: 0,
                event: SessionEvent::QuestionAsked {
                    question: q,
                    inputs: vec![q.reply_to],
                    text: "Which scope?".into(),
                },
            }],
        );
        let (text, hits, _) = render_rows(&state, 8);
        assert_eq!(text.matches("Which scope?").count(), 1);
        assert_eq!(
            hits.iter()
                .filter(|hit| hit.target == ClickTarget::Action(Action::AnswerQuestion(q)))
                .count(),
            1
        );
    }

    #[test]
    fn recovery_and_unknown_submission_are_visible_without_history() {
        let (mut state, _) = fixture();
        let ui = state.selected_ui_mut().unwrap();
        Arc::make_mut(ui.snapshot.as_mut().unwrap()).inputs.extend([
            InputView {
                id: InputId(2),
                request_id: RequestId::new(),
                text: "queued".into(),
                reply_to: None,
                state: InputState::Queued { problem: None },
            },
            InputView {
                id: InputId(3),
                request_id: RequestId::new(),
                text: "rejected".into(),
                reply_to: None,
                state: InputState::Rejected {
                    message: "invalid".into(),
                },
            },
        ]);
        ui.submitting = Some(PendingSubmission {
            request_id: RequestId::new(),
            text: "unconfirmed".into(),
            draft_revision: 0,
            failed: true,
            reply_to: None,
        });
        let (text, hits, _) = render_rows(&state, 8);
        assert!(text.contains("Retry saved input #2"));
        assert!(text.contains("Restore input #3 to draft"));
        assert!(text.contains("Retry original submission"));
        for target in [
            ClickTarget::Action(Action::RetryInput(InputId(2))),
            ClickTarget::Action(Action::RestoreInput(InputId(3))),
            ClickTarget::Action(Action::RetrySubmission),
        ] {
            assert!(hits.iter().any(|h| h.target == target));
        }
    }

    #[test]
    fn older_history_does_not_receive_live_actions_or_jobs() {
        let (mut state, q) = fixture();
        let ui = state.selected_ui_mut().unwrap();
        let snapshot = Arc::make_mut(ui.snapshot.as_mut().unwrap());
        snapshot.inputs.push(active_input(q));
        snapshot.inputs.push(InputView {
            id: InputId(2),
            request_id: RequestId::new(),
            text: "queued".into(),
            reply_to: None,
            state: InputState::Queued { problem: None },
        });
        snapshot.jobs.push(JobView {
            allowed_tools: ["read".to_owned(), "glob".to_owned()].into(),
            id: JobRef {
                runtime: q.runtime,
                id: 9,
            },
            owner: JobOwner::User,
            inputs: vec![InputId(1)],
            goal: "Live job".into(),
            scope: String::new(),
            done_when: String::new(),
            state: JobState::Running,
            report: None,
        });
        ui.submitting = Some(PendingSubmission {
            request_id: RequestId::new(),
            text: "unconfirmed".into(),
            draft_revision: 0,
            failed: true,
            reply_to: None,
        });
        replace_history(
            ui,
            vec![HistoryEntry {
                sequence: SessionSeq(1),
                occurred_at: 0,
                event: SessionEvent::Reply {
                    inputs: vec![InputId(1)],
                    text: (0..30).map(|i| format!("History row {i}\n")).collect(),
                },
            }],
        );
        ui.transcript.scroll_up(1);
        let (text, hits, _) = render_rows(&state, 8);
        assert!(!text.contains("Live job"));
        assert!(!text.contains("Which scope?"));
        assert!(!text.contains("Retry"));
        assert!(
            hits.is_empty(),
            "live action links must not move into old history"
        );
    }

    #[test]
    fn expired_historical_questions_have_no_answer_hit() {
        let (mut state, q) = fixture();
        let ui = state.selected_ui_mut().unwrap();
        replace_history(
            ui,
            vec![HistoryEntry {
                sequence: SessionSeq(1),
                occurred_at: 0,
                event: SessionEvent::QuestionAsked {
                    question: q,
                    inputs: vec![InputId(1)],
                    text: "Old scope question".into(),
                },
            }],
        );
        let (text, hits, _) = render_rows(&state, 8);
        assert!(text.contains("Old scope question"));
        assert!(
            !hits
                .iter()
                .any(|h| matches!(h.target, ClickTarget::Action(Action::AnswerQuestion(_))))
        );
        Arc::make_mut(state.selected_ui_mut().unwrap().snapshot.as_mut().unwrap())
            .inputs
            .push(active_input(q));
        let (_, hits, _) = render_rows(&state, 8);
        assert!(
            hits.iter()
                .any(|h| h.target == ClickTarget::Action(Action::AnswerQuestion(q)))
        );
    }

    #[test]
    fn active_job_detail_hit_tracks_the_visible_job_row() {
        let (mut state, q) = fixture();
        let job = JobRef {
            runtime: q.runtime,
            id: 42,
        };
        Arc::make_mut(state.selected_ui_mut().unwrap().snapshot.as_mut().unwrap())
            .jobs
            .push(JobView {
                allowed_tools: ["read".to_owned(), "glob".to_owned()].into(),
                id: job,
                owner: JobOwner::User,
                inputs: vec![InputId(1)],
                goal: "Check draft recovery".into(),
                scope: String::new(),
                done_when: String::new(),
                state: JobState::Running,
                report: None,
            });
        let (text, hits, _) = render_rows(&state, 4);
        let hit = hits
            .iter()
            .find(|h| h.target == ClickTarget::Action(Action::OpenJob(job)))
            .expect("job details hit");
        assert!(
            text.lines()
                .nth(usize::from(hit.area.y))
                .unwrap()
                .contains("Check draft recovery")
        );
        assert!(hit.area.bottom() <= 4);
    }

    #[test]
    fn every_row_of_a_long_message_is_reachable() {
        let rows = (0..40)
            .map(|index| Line::from(index.to_string()))
            .collect::<Vec<_>>();
        assert_eq!(visible_rows(&rows, 10, 0)[0].to_string(), "30");
        assert_eq!(visible_rows(&rows, 10, 30)[0].to_string(), "0");
    }
}
