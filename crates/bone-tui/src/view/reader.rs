//! Pure reader rendering, shared by the right rail and narrow view.
//!
//! The caller owns selection, loading, focus and scroll. This module performs no
//! App calls and never interprets tool text as a status or a command.
use crate::state::reader::ReaderContent;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::Line,
    widgets::{Block, Paragraph},
};

use super::{ACCENT, INK, MUTED, RAIL};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ReaderMetrics {
    /// Scroll offsets count wrapped terminal rows, not bytes or source lines.
    pub scroll: usize,
    pub max_scroll: usize,
    pub total_rows: usize,
    pub body: Rect,
    /// The caller can register a pointer target for returning to the source.
    pub back: Rect,
}

/// Render in any caller-provided area. The caller must retain the returned clamp
/// after resize and route Esc locally before global execution-stop handling.
pub(crate) fn render(
    frame: &mut Frame<'_>,
    area: Rect,
    content: &ReaderContent,
    snapshot: Option<&bone_app::SessionView>,
    scroll: usize,
    focused: bool,
) -> ReaderMetrics {
    if area.is_empty() {
        return ReaderMetrics::default();
    }
    frame.render_widget(Block::default().style(Style::default().bg(RAIL)), area);
    let padding = u16::from(area.width >= 8) * 3;
    let inner = Rect::new(
        area.x + padding,
        area.y + u16::from(area.height > 1),
        area.width.saturating_sub(padding * 2),
        area.height.saturating_sub(1),
    );
    let back = Rect::new(inner.x, area.bottom() - 1, inner.width, 1);
    if inner.height > 1 {
        let title = super::single_line_external(&content.title);
        frame.render_widget(
            Paragraph::new(format!("{}{title}", if focused { "› " } else { "" })).style(
                Style::default()
                    .fg(if focused { ACCENT } else { INK })
                    .add_modifier(Modifier::BOLD),
            ),
            Rect::new(inner.x, inner.y, inner.width, 1),
        );
    }
    let body = Rect::new(
        inner.x,
        (inner.y + 2).min(inner.bottom()),
        inner.width,
        inner.height.saturating_sub(3),
    );
    let rows = content.wrapped_rows(usize::from(inner.width));
    let inputs = content.job_inputs(snapshot);
    let width = usize::from(inner.width).max(1);
    let input_heading = inputs
        .map(|inputs| crate::text::wrap_text(&format!("Inputs ({})", inputs.len()), width))
        .unwrap_or_default();
    let input_row_height = 20usize.div_ceil(width);
    let tail_rows = inputs.map_or(0, |inputs| {
        1usize
            .saturating_add(input_heading.len())
            .saturating_add(inputs.len().saturating_mul(input_row_height))
    });
    let total_rows = rows.len().saturating_add(tail_rows);
    let max_scroll = if body.height == 0 {
        0
    } else {
        total_rows.saturating_sub(usize::from(body.height))
    };
    let scroll = scroll.min(max_scroll);
    let mut visible: Vec<Line<'_>> = rows
        .iter()
        .skip(scroll)
        .take(usize::from(body.height))
        .map(Line::raw)
        .collect();
    if let Some(inputs) = inputs {
        for row in (scroll + visible.len())
            ..(scroll
                .saturating_add(usize::from(body.height))
                .min(total_rows))
        {
            let tail = row - rows.len();
            if tail == 0 {
                visible.push(Line::raw(""));
            } else if let Some(heading) = input_heading.get(tail - 1) {
                visible.push(Line::raw(heading.as_str()));
            } else {
                let offset = tail - 1 - input_heading.len();
                let index = offset / input_row_height;
                let part = offset % input_row_height;
                visible.push(Line::raw(input_id_fragment(inputs[index], width, part)));
            }
        }
    }
    frame.render_widget(
        Paragraph::new(visible).style(Style::default().fg(INK)),
        body,
    );
    let footer = if inner.width >= 30 && max_scroll > 0 {
        format!(
            "esc back · {}–{} / {}",
            scroll + 1,
            (scroll + usize::from(body.height)).min(total_rows),
            total_rows
        )
    } else {
        "esc back".to_owned()
    };
    frame.render_widget(
        Paragraph::new(footer).style(Style::default().fg(MUTED)),
        back,
    );
    ReaderMetrics {
        scroll,
        max_scroll,
        total_rows,
        body,
        back,
    }
}

// A u64 occupies at most 20 decimal columns. Fixed row height gives direct
// indexing at any scroll offset, including terminals narrower than one ID.
fn input_id_fragment(id: bone_app::InputId, width: usize, part: usize) -> String {
    let text = format!("{:<20}", id.0);
    let start = part * width;
    text[start..(start + width).min(20)].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::reader::ReaderSource;
    use bone_app::{
        CallRef, ExternalEffect, HistoryEntry, JobOwner, JobRef, JobReport, JobState, JobView,
        RuntimeId, RuntimeState, SessionEvent, SessionId, SessionInfo, SessionSeq, SessionView,
        ToolOutcome, WorkspaceId,
    };
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn input_id_fragments_preserve_every_u64_digit_at_all_widths() {
        for id in [0, 1, u64::MAX] {
            for width in 1..=40 {
                let rendered = (0..20usize.div_ceil(width))
                    .map(|part| input_id_fragment(bone_app::InputId(id), width, part))
                    .collect::<String>();
                assert_eq!(rendered.trim_end(), id.to_string());
            }
        }
    }

    #[test]
    fn million_job_inputs_remain_borrowed_and_tail_refresh_and_expiry_are_exact() {
        let id = JobRef {
            runtime: RuntimeId::new(),
            id: 7,
        };
        let mut snapshot = SessionView {
            session: SessionInfo {
                id: SessionId::new(),
                workspace: WorkspaceId::new(),
                title: "large job".into(),
                archived: false,
            },
            runtime: RuntimeState::Detached,
            draft: String::new(),
            inputs: vec![],
            activity: vec![],
            history_through: SessionSeq(0),
            problem: None,
            jobs: vec![JobView {
                id,
                owner: JobOwner::User,
                inputs: (0..1_000_000)
                    .map(|index| bone_app::InputId(u64::MAX - 999_999 + index))
                    .collect(),
                goal: "goal".into(),
                scope: "scope".into(),
                done_when: "done".into(),
                state: JobState::Running,
                report: None,
            }],
        };
        let mut content = ReaderContent::from_job(&snapshot, id).unwrap();
        assert!(content.text.len() < 1024);
        assert_eq!(
            content.job_inputs(Some(&snapshot)).unwrap().as_ptr(),
            snapshot.jobs[0].inputs.as_ptr()
        );
        let cached = content.wrapped_rows(74);
        assert!(cached.allocated_bytes() < 4096);
        let mut terminal = Terminal::new(TestBackend::new(80, 10)).unwrap();
        let mut metrics = ReaderMetrics::default();
        terminal
            .draw(|frame| {
                metrics = render(
                    frame,
                    frame.area(),
                    &content,
                    Some(&snapshot),
                    usize::MAX,
                    true,
                )
            })
            .unwrap();
        assert_eq!(metrics.total_rows, cached.len() + 2 + 1_000_000);
        let visible: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(visible.contains(&u64::MAX.to_string()));
        // Changing associations does not duplicate the source or invalidate body layout.
        snapshot.jobs[0].inputs.push(bone_app::InputId(0));
        content.refresh_job(&snapshot);
        assert!(std::sync::Arc::ptr_eq(&cached, &content.wrapped_rows(74)));
        terminal
            .draw(|frame| {
                metrics = render(
                    frame,
                    frame.area(),
                    &content,
                    Some(&snapshot),
                    usize::MAX,
                    true,
                )
            })
            .unwrap();
        assert_eq!(metrics.total_rows, cached.len() + 2 + 1_000_001);
        assert_eq!(
            content.job_inputs(Some(&snapshot)).unwrap().last(),
            Some(&bone_app::InputId(0))
        );
        snapshot.jobs[0].id.runtime = RuntimeId::new();
        content.refresh_job(&snapshot);
        assert!(content.job_inputs(Some(&snapshot)).is_none());
        assert_eq!(content.source, ReaderSource::Job(id));
        assert!(content.text.contains("no longer in the current snapshot"));
        terminal
            .draw(|frame| {
                metrics = render(
                    frame,
                    frame.area(),
                    &content,
                    Some(&snapshot),
                    usize::MAX,
                    true,
                )
            })
            .unwrap();
        assert!(metrics.total_rows < 20);
        snapshot.jobs[0].id = id;
        snapshot.jobs[0].inputs.clear();
        content.refresh_job(&snapshot);
        let body_rows = content.wrapped_rows(74).len();
        terminal
            .draw(|frame| {
                metrics = render(
                    frame,
                    frame.area(),
                    &content,
                    Some(&snapshot),
                    usize::MAX,
                    true,
                )
            })
            .unwrap();
        assert_eq!(metrics.total_rows, body_rows + 2);
        terminal
            .draw(|frame| metrics = render(frame, frame.area(), &content, None, usize::MAX, true))
            .unwrap();
        assert_eq!(metrics.total_rows, body_rows);
        snapshot.jobs[0].inputs.push(bone_app::InputId(u64::MAX));
        snapshot.session.id = SessionId::new();
        terminal
            .draw(|frame| {
                metrics = render(
                    frame,
                    frame.area(),
                    &content,
                    Some(&snapshot),
                    usize::MAX,
                    true,
                )
            })
            .unwrap();
        assert_eq!(metrics.total_rows, body_rows);
        snapshot.session.id = content.session;
        for width in [1, 2, 8, 40] {
            let mut terminal = Terminal::new(TestBackend::new(width, 30)).unwrap();
            terminal
                .draw(|frame| {
                    render(
                        frame,
                        frame.area(),
                        &content,
                        Some(&snapshot),
                        usize::MAX,
                        true,
                    );
                })
                .unwrap();
            let visible: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(
                visible.replace(' ', "").contains(&u64::MAX.to_string()),
                "width={width}"
            );
        }
    }

    fn tool(outcome: ToolOutcome) -> HistoryEntry {
        let runtime = RuntimeId::new();
        HistoryEntry {
            sequence: SessionSeq(17),
            occurred_at: 0,
            event: SessionEvent::ToolFinished {
                call: CallRef { runtime, id: 2 },
                job: JobRef { runtime, id: 7 },
                tool: "read".into(),
                outcome,
            },
        }
    }

    #[test]
    #[ignore = "release rendering measurement"]
    fn one_mib_reader_scroll_measurement() {
        let text = "0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ\n".repeat(16645);
        let content = ReaderContent {
            layout_cache: Default::default(),
            session: SessionId::new(),
            source: ReaderSource::History(SessionSeq(1)),
            title: "1 MiB output".into(),
            text: text.into(),
        };
        assert!(content.text.len() >= 1024 * 1024);
        let mut terminal = Terminal::new(TestBackend::new(80, 30)).unwrap();
        let first = std::time::Instant::now();
        terminal
            .draw(|frame| {
                render(frame, frame.area(), &content, None, 0, true);
            })
            .unwrap();
        let first = first.elapsed();
        let rows = content.wrapped_rows(74);
        let allocated = rows.allocated_bytes();
        assert!(content.layout_cache.borrow().is_some());
        assert!(allocated <= 8 * 1024 * 1024);
        eprintln!(
            "cached layout bytes={allocated}, content bytes={}",
            content.text.len()
        );
        let start = std::time::Instant::now();
        for scroll in 1..=30 {
            terminal
                .draw(|frame| {
                    render(frame, frame.area(), &content, None, scroll, true);
                })
                .unwrap();
        }
        eprintln!(
            "reader bytes={} first={:?} scroll_30={:?} per_scroll={:?}",
            content.text.len(),
            first,
            start.elapsed(),
            start.elapsed() / 30
        );
    }

    #[test]
    #[ignore = "release rendering measurement"]
    fn one_mib_newlines_reader_scroll_measurement() {
        let text = "\n".repeat(1024 * 1024);
        let content = ReaderContent {
            layout_cache: Default::default(),
            session: SessionId::new(),
            source: ReaderSource::History(SessionSeq(1)),
            title: "1 MiB output".into(),
            text: text.into(),
        };
        assert!(content.text.len() >= 1024 * 1024);
        let mut terminal = Terminal::new(TestBackend::new(80, 30)).unwrap();
        let first = std::time::Instant::now();
        terminal
            .draw(|frame| {
                render(frame, frame.area(), &content, None, 0, true);
            })
            .unwrap();
        let first = first.elapsed();
        let rows = content.wrapped_rows(74);
        let allocated = rows.allocated_bytes();
        assert!(content.layout_cache.borrow().is_some());
        assert!(allocated <= 8 * 1024 * 1024);
        eprintln!(
            "cached layout bytes={allocated}, content bytes={}",
            content.text.len()
        );
        let start = std::time::Instant::now();
        for scroll in 1..=30 {
            terminal
                .draw(|frame| {
                    render(frame, frame.area(), &content, None, scroll, true);
                })
                .unwrap();
        }
        eprintln!(
            "reader bytes={} first={:?} scroll_30={:?} per_scroll={:?}",
            content.text.len(),
            first,
            start.elapsed(),
            start.elapsed() / 30
        );
    }

    #[test]
    fn complete_tool_result_and_external_effect_are_kept() {
        let value = format!("first\n{}\nLAST", "汉字\t".repeat(2_000));
        let entry = tool(ToolOutcome {
            result: Ok(value.clone().into()),
            external_effect: ExternalEffect::Unknown,
        });
        let session = SessionId::new();
        let content = ReaderContent::from_history(session, &entry).unwrap();
        assert_eq!(content.session, session);
        assert_eq!(content.source, ReaderSource::History(SessionSeq(17)));
        assert!(content.text.contains(&value));
        assert!(content.text.contains("External effect: Unknown"));
        let failed =
            ReaderContent::from_history(session, &tool(ToolOutcome::failed("full\nerror")))
                .unwrap();
        assert!(failed.text.contains("Kind: Failed\n\nfull\nerror"));
    }

    #[test]
    fn job_lookup_uses_full_runtime_identity_and_retains_public_fields() {
        let id = JobRef {
            runtime: RuntimeId::new(),
            id: 7,
        };
        let view = SessionView {
            session: SessionInfo {
                id: SessionId::new(),
                workspace: WorkspaceId::new(),
                title: "session".into(),
                archived: false,
            },
            runtime: RuntimeState::Detached,
            draft: String::new(),
            inputs: vec![],
            activity: vec![],
            history_through: SessionSeq(0),
            problem: None,
            jobs: vec![JobView {
                id,
                owner: JobOwner::User,
                inputs: vec![bone_app::InputId(4)],
                goal: "goal".into(),
                scope: "scope".into(),
                done_when: "done".into(),
                state: JobState::Paused,
                report: Some(JobReport {
                    summary: "report".into(),
                }),
            }],
        };
        assert!(
            ReaderContent::from_job(
                &view,
                JobRef {
                    runtime: RuntimeId::new(),
                    id: 7
                }
            )
            .is_none()
        );
        let mut content = ReaderContent::from_job(&view, id).unwrap();
        for value in ["goal", "scope", "done", "Paused", "report"] {
            assert!(content.text.contains(value), "missing {value}");
        }
        let before = content.wrapped_rows(36);
        content.refresh_job(&view);
        assert!(std::sync::Arc::ptr_eq(&before, &content.wrapped_rows(36)));
        let mut changed = view.clone();
        changed.jobs[0].state = JobState::Running;
        content.refresh_job(&changed);
        assert!(!std::sync::Arc::ptr_eq(&before, &content.wrapped_rows(36)));
        assert!(content.wrapped_rows(36).iter().any(|row| row == "Running"));
        changed.jobs.clear();
        content.refresh_job(&changed);
        assert!(
            content
                .wrapped_rows(80)
                .iter()
                .any(|row| row.contains("no longer"))
        );
    }

    #[test]
    fn reader_layout_reuses_only_the_current_text_and_width() {
        let mut content = ReaderContent {
            layout_cache: Default::default(),
            session: SessionId::new(),
            source: ReaderSource::History(SessionSeq(1)),
            title: "output".into(),
            text: "中文 output\nsecond line".into(),
        };
        let original = content.wrapped_rows(36);
        assert!(std::sync::Arc::ptr_eq(&original, &content.wrapped_rows(36)));
        let narrow = content.wrapped_rows(12);
        assert!(!std::sync::Arc::ptr_eq(&original, &narrow));
        assert!(!std::sync::Arc::ptr_eq(
            &original,
            &content.wrapped_rows(36)
        ));
        content.text = "updated output".into();
        let updated = content.wrapped_rows(36);
        assert_eq!(updated.iter().collect::<Vec<_>>(), ["updated output"]);
        assert!(std::sync::Arc::ptr_eq(&updated, &content.wrapped_rows(36)));
    }

    #[test]
    fn scrolling_reaches_tail_beyond_u16_and_resize_clamps() {
        let content = ReaderContent {
            layout_cache: Default::default(),
            session: SessionId::new(),
            source: ReaderSource::History(SessionSeq(1)),
            title: "output".into(),
            text: format!("{}THE END", "row\n".repeat(65_550)).into(),
        };
        let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();
        let mut metrics = ReaderMetrics::default();
        terminal
            .draw(|frame| metrics = render(frame, frame.area(), &content, None, usize::MAX, true))
            .unwrap();
        assert!(metrics.scroll > usize::from(u16::MAX));
        let rendered: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(rendered.contains("THE END"));
        assert_eq!(metrics.scroll, metrics.max_scroll);
        let short = ReaderContent {
            text: "short".into(),
            ..content
        };
        terminal
            .draw(|frame| metrics = render(frame, frame.area(), &short, None, metrics.scroll, true))
            .unwrap();
        assert_eq!(metrics.scroll, 0);
    }

    #[test]
    fn external_controls_and_tiny_areas_do_not_escape_the_reader() {
        let content = ReaderContent {
            layout_cache: Default::default(),
            session: SessionId::new(),
            source: ReaderSource::History(SessionSeq(1)),
            title: "bad\n\u{1b}title".into(),
            text: "中文\twide\u{202e}\u{7}\nnext".into(),
        };
        for (width, height) in [(1, 1), (2, 2), (8, 3), (40, 12), (80, 24)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| {
                    render(frame, frame.area(), &content, None, 0, true);
                })
                .unwrap();
            for cell in &terminal.backend().buffer().content {
                assert!(
                    !cell
                        .symbol()
                        .chars()
                        .any(|ch| ch.is_control() || ch == '\u{202e}')
                );
            }
        }
    }
}
