//! Pure reader panel rendering.
//!
//! The caller owns selection, loading, focus and scroll. This module performs no
//! App calls and never interprets tool text as a status or a command.
use crate::{
    state::reader::ReaderContent,
    ui::{
        selection::{CopySource, SelectableText, TextRow},
        theme,
    },
};
use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph},
};

const TITLE_INSET: u16 = 2;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ReaderMetrics {
    pub max_scroll: usize,
    pub body: Rect,
    /// The caller can register a pointer target for returning to the source.
    pub back: Rect,
    pub texts: Vec<SelectableText>,
}

/// Render in any caller-provided area. The caller must retain the returned clamp
/// after resize and provide a local close action.
pub(crate) fn render(
    frame: &mut Frame<'_>,
    area: Rect,
    content: &ReaderContent,
    snapshot: Option<&bone_app::SessionView>,
    scroll: usize,
) -> ReaderMetrics {
    if area.is_empty() {
        return ReaderMetrics::default();
    }
    frame.render_widget(Clear, area);
    frame.render_widget(
        Block::default().style(Style::default().bg(theme::RAIL)),
        area,
    );
    let padding = u16::from(area.width >= 8) * 3;
    let spacious = area.height >= 5;
    let top_padding = u16::from(spacious);
    let inner = Rect::new(
        area.x + padding,
        area.y + top_padding,
        area.width.saturating_sub(padding * 2),
        area.height.saturating_sub(top_padding),
    );
    let back = Rect::new(inner.x, area.bottom() - 1, inner.width, 1);
    let show_title = area.height >= 3;
    if show_title {
        let title = super::single_line_external(&content.title);
        let title_row = Rect::new(inner.x, inner.y, inner.width, 1);
        let title_background = theme::SELECTED;
        frame.render_widget(
            Block::default().style(theme::surface(title_background)),
            title_row,
        );
        let gutter_width = TITLE_INSET.min(title_row.width);
        frame.render_widget(
            Paragraph::new(title).style(theme::label_on(theme::INK, title_background)),
            Rect::new(
                title_row.x + gutter_width,
                title_row.y,
                title_row.width.saturating_sub(gutter_width),
                1,
            ),
        );
    }
    let body_y = inner.y + u16::from(show_title) + u16::from(spacious);
    let body = Rect::new(inner.x, body_y, inner.width, back.y.saturating_sub(body_y));
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
    let source = CopySource::Details {
        session: content.session,
        source: content.source,
    };
    let mut texts = vec![SelectableText {
        source,
        item: 0,
        text: content.text.clone(),
        rows: (scroll..(scroll + usize::from(body.height)).min(rows.len()))
            .map(|row| {
                TextRow::new(
                    Rect::new(
                        body.x + rows.gutter,
                        body.y + (row - scroll) as u16,
                        body.width.saturating_sub(rows.gutter),
                        1,
                    ),
                    &content.text,
                    rows.range(row),
                )
            })
            .collect(),
    }];
    let mut visible: Vec<Line<'_>> = rows
        .iter()
        .enumerate()
        .skip(scroll)
        .take(usize::from(body.height))
        .map(|(row, text)| {
            if rows.gutter == 0 {
                return Line::raw(text);
            }
            let number = rows
                .line_number(row)
                .map(|number| number.to_string())
                .unwrap_or_default();
            Line::from(vec![
                Span::styled(
                    format!(
                        "{number:>width$} ",
                        width = usize::from(rows.gutter.saturating_sub(1))
                    ),
                    theme::body(theme::MUTED),
                ),
                Span::raw(text),
            ])
        })
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
                let value = format!("Inputs ({})", inputs.len());
                let heading_starts = crate::ui::selection::source_starts(&value, width);
                let range = crate::ui::selection::row_range(&value, &heading_starts, tail - 1);
                let mapped = TextRow::new(
                    Rect::new(body.x, body.y + (row - scroll) as u16, body.width, 1),
                    &value,
                    range,
                );
                if let Some(text) = texts.last_mut().filter(|text| text.item == 1) {
                    text.rows.push(mapped);
                } else {
                    texts.push(SelectableText {
                        source,
                        item: 1,
                        text: value.into(),
                        rows: vec![mapped],
                    });
                }
                visible.push(Line::raw(heading.as_str()));
            } else {
                let offset = tail - 1 - input_heading.len();
                let index = offset / input_row_height;
                let part = offset % input_row_height;
                let item = index as u64 + 2;
                let value = inputs[index].0.to_string();
                let start = (part * width).min(value.len());
                let end = ((part + 1) * width).min(value.len());
                let text_row = TextRow::new(
                    Rect::new(body.x, body.y + (row - scroll) as u16, body.width, 1),
                    &value,
                    start..end,
                );
                if let Some(text) = texts.last_mut().filter(|text| text.item == item) {
                    text.rows.push(text_row);
                } else {
                    texts.push(SelectableText {
                        source,
                        item,
                        text: value.into(),
                        rows: vec![text_row],
                    });
                }
                visible.push(Line::raw(input_id_fragment(inputs[index], width, part)));
            }
        }
    }
    frame.render_widget(
        Paragraph::new(visible).style(Style::default().fg(theme::INK)),
        body,
    );
    let footer = if inner.width >= 30 && max_scroll > 0 {
        format!(
            "close details · {}–{} / {}",
            scroll + 1,
            (scroll + usize::from(body.height)).min(total_rows),
            total_rows
        )
    } else {
        "close details".to_owned()
    };
    frame.render_widget(
        Paragraph::new(footer).style(Style::default().fg(theme::MUTED)),
        back,
    );
    ReaderMetrics {
        max_scroll,
        body,
        back,
        texts,
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
    use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, style::Modifier};

    fn find_text(buffer: &Buffer, needle: &str) -> Option<(u16, u16)> {
        for y in buffer.area.y..buffer.area.bottom() {
            let line = (buffer.area.x..buffer.area.right())
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>();
            if let Some(x) = line.find(needle) {
                return Some((buffer.area.x + x as u16, y));
            }
        }
        None
    }

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
    fn smallest_workspace_shows_and_scrolls_details_without_covering_the_composer() {
        use crate::{
            layout::ClickTarget,
            state::{Action, ReaderState, UiState, update},
        };
        use crossterm::event::{Event, KeyModifiers, MouseEvent, MouseEventKind};

        let mut state = UiState::default();
        state.orphan_draft = "continue typing".into();
        state.details = Some(ReaderState {
            content: ReaderContent {
                numbered: Vec::new(),
                layout_cache: Default::default(),
                session: SessionId::new(),
                source: ReaderSource::History(SessionSeq(1)),
                title: "Detail title".into(),
                text: (0..12)
                    .map(|index| format!("line{index:02}\n"))
                    .collect::<String>()
                    .into(),
            },
            scroll: 0,
        });
        let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
        let mut snapshot = None;
        terminal
            .draw(|frame| snapshot = Some(crate::view::render(frame, &state)))
            .unwrap();
        assert!(find_text(terminal.backend().buffer(), "line00").is_some());
        assert!(find_text(terminal.backend().buffer(), "continue typing").is_some());
        let snapshot = snapshot.unwrap();
        let area = snapshot.layout.details_area().unwrap();
        assert!(area.bottom() <= snapshot.layout.composer.unwrap().y);
        assert!(
            snapshot
                .hit_regions()
                .iter()
                .any(|region| region.target == ClickTarget::Action(Action::CloseDetails))
        );
        let event = crate::input::terminal_event(
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: area.x + 3,
                row: area.y + 1,
                modifiers: KeyModifiers::NONE,
            }),
            Some(&snapshot),
            &state,
        )
        .expect("details wheel input");
        update(&mut state, event);
        assert_eq!(state.details.as_ref().unwrap().scroll, 3);
        terminal
            .draw(|frame| {
                crate::view::render(frame, &state);
            })
            .unwrap();
        assert!(find_text(terminal.backend().buffer(), "line03").is_some());
        assert!(find_text(terminal.backend().buffer(), "line00").is_none());
        assert_eq!(state.draft(), "continue typing");
    }

    #[test]
    fn reader_title_uses_a_neutral_surface_without_orange() {
        let content = ReaderContent {
            numbered: Vec::new(),
            layout_cache: Default::default(),
            session: SessionId::new(),
            source: ReaderSource::History(SessionSeq(1)),
            title: "Reader title".into(),
            text: "body".into(),
        };
        let mut active = Terminal::new(TestBackend::new(48, 12)).unwrap();
        active
            .draw(|frame| {
                super::render(frame, frame.area(), &content, None, 0);
            })
            .unwrap();
        let active_buffer = active.backend().buffer();
        let active_title = find_text(active_buffer, "Reader title").expect("active title");
        let (title_x, title_y) = active_title;
        let mark_x = title_x - TITLE_INSET;
        let active_mark = &active_buffer[(mark_x, title_y)];
        assert_eq!(active_mark.symbol(), " ");
        assert_eq!(active_mark.bg, theme::SELECTED);
        assert_ne!(active_mark.bg, theme::FOCUS_MARK);
        assert!(
            !active_mark
                .modifier
                .contains(Modifier::BOLD | Modifier::DIM)
        );
        assert_eq!(active_buffer[(mark_x + 1, title_y)].bg, theme::SELECTED);
        assert_eq!(active_buffer[(title_x, title_y)].bg, theme::SELECTED);
        assert_eq!(active_buffer[(title_x, title_y)].fg, theme::INK);
        assert!(
            active_buffer[(title_x, title_y)]
                .modifier
                .contains(Modifier::BOLD)
        );
        assert!(
            active_buffer
                .content()
                .iter()
                .all(|cell| cell.fg != theme::FOCUS_MARK && cell.bg != theme::FOCUS_MARK)
        );
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
                allowed_tools: ["read".to_owned(), "glob".to_owned()].into(),
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
                metrics = render(frame, frame.area(), &content, Some(&snapshot), usize::MAX)
            })
            .unwrap();
        assert_eq!(
            metrics.max_scroll,
            (cached.len() + 2 + 1_000_000).saturating_sub(usize::from(metrics.body.height))
        );
        let visible: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(visible.contains(&u64::MAX.to_string()));
        // Selection resolves offscreen virtual IDs from the same snapshot without
        // cloning the million-element list or serializing the whole document.
        snapshot = {
            use crate::ui::{
                interaction::{FrameHits, SurfaceHits},
                selection::TextPoint,
            };
            let source = CopySource::Details {
                session: content.session,
                source: content.source,
            };
            let input_pointer = snapshot.jobs[0].inputs.as_ptr();
            let shared = std::sync::Arc::new(snapshot);
            {
                let mut hits = SurfaceHits::default();
                for text in metrics.texts.iter().cloned() {
                    hits.push_text(text);
                }
                hits.push_job_inputs(source, shared.clone());
                let hits = FrameHits::new(hits, None);
                let first = TextPoint {
                    source,
                    item: 2,
                    byte: 0,
                };
                let third = TextPoint {
                    source,
                    item: 4,
                    byte: 20,
                };
                assert_eq!(
                    hits.source_content(first).as_deref(),
                    Some((u64::MAX - 999_999).to_string().as_str())
                );
                assert_eq!(
                    hits.copy_between(first, third),
                    Some(
                        (0..3)
                            .map(|i| (u64::MAX - 999_999 + i).to_string())
                            .collect::<Vec<_>>()
                            .join("\n")
                    )
                );
                assert_eq!(shared.jobs[0].inputs.as_ptr(), input_pointer);
            }
            std::sync::Arc::try_unwrap(shared).unwrap()
        };
        // Changing associations does not duplicate the source or invalidate body layout.
        snapshot.jobs[0].inputs.push(bone_app::InputId(0));
        content.refresh_job(&snapshot);
        assert!(std::sync::Arc::ptr_eq(&cached, &content.wrapped_rows(74)));
        terminal
            .draw(|frame| {
                metrics = render(frame, frame.area(), &content, Some(&snapshot), usize::MAX)
            })
            .unwrap();
        assert_eq!(
            metrics.max_scroll,
            (cached.len() + 2 + 1_000_001).saturating_sub(usize::from(metrics.body.height))
        );
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
                metrics = render(frame, frame.area(), &content, Some(&snapshot), usize::MAX)
            })
            .unwrap();
        assert!(metrics.max_scroll < 20);
        snapshot.jobs[0].id = id;
        snapshot.jobs[0].inputs.clear();
        content.refresh_job(&snapshot);
        let body_rows = content.wrapped_rows(74).len();
        terminal
            .draw(|frame| {
                metrics = render(frame, frame.area(), &content, Some(&snapshot), usize::MAX)
            })
            .unwrap();
        assert_eq!(
            metrics.max_scroll,
            (body_rows + 2).saturating_sub(usize::from(metrics.body.height))
        );
        terminal
            .draw(|frame| metrics = render(frame, frame.area(), &content, None, usize::MAX))
            .unwrap();
        assert_eq!(
            metrics.max_scroll,
            body_rows.saturating_sub(usize::from(metrics.body.height))
        );
        snapshot.jobs[0].inputs.push(bone_app::InputId(u64::MAX));
        snapshot.session.id = SessionId::new();
        terminal
            .draw(|frame| {
                metrics = render(frame, frame.area(), &content, Some(&snapshot), usize::MAX)
            })
            .unwrap();
        assert_eq!(
            metrics.max_scroll,
            body_rows.saturating_sub(usize::from(metrics.body.height))
        );
        snapshot.session.id = content.session;
        for width in [1, 2, 8, 40] {
            let mut terminal = Terminal::new(TestBackend::new(width, 30)).unwrap();
            terminal
                .draw(|frame| {
                    render(frame, frame.area(), &content, Some(&snapshot), usize::MAX);
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
                arguments: serde_json::json!({}),
                outcome,
            },
        }
    }

    #[test]
    #[ignore = "release rendering measurement"]
    fn one_mib_reader_scroll_measurement() {
        let text = "0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ\n".repeat(16645);
        let content = ReaderContent {
            numbered: Vec::new(),
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
                render(frame, frame.area(), &content, None, 0);
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
                    render(frame, frame.area(), &content, None, scroll);
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
            numbered: Vec::new(),
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
                render(frame, frame.area(), &content, None, 0);
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
                    render(frame, frame.area(), &content, None, scroll);
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
        assert!(content.text.contains("External effects are unknown"));
        let failed =
            ReaderContent::from_history(session, &tool(ToolOutcome::failed("full\nerror")))
                .unwrap();
        assert!(failed.text.contains("Error\nfull\nerror"));
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
                allowed_tools: ["read".to_owned(), "glob".to_owned()].into(),
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
        for value in [
            "goal",
            "scope",
            "done",
            "Paused",
            "report",
            "Allowed tools\nglob, read",
        ] {
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
        changed.jobs[0].allowed_tools.clear();
        content.refresh_job(&changed);
        assert!(content.text.contains("Allowed tools\nNone"));
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
            numbered: Vec::new(),
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
            numbered: Vec::new(),
            layout_cache: Default::default(),
            session: SessionId::new(),
            source: ReaderSource::History(SessionSeq(1)),
            title: "output".into(),
            text: format!("{}THE END", "row\n".repeat(65_550)).into(),
        };
        let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();
        let mut metrics = ReaderMetrics::default();
        terminal
            .draw(|frame| metrics = render(frame, frame.area(), &content, None, usize::MAX))
            .unwrap();
        assert!(metrics.max_scroll > usize::from(u16::MAX));
        let rendered: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(rendered.contains("THE END"));
        let short = ReaderContent {
            text: "short".into(),
            ..content
        };
        terminal
            .draw(|frame| metrics = render(frame, frame.area(), &short, None, metrics.max_scroll))
            .unwrap();
        assert_eq!(metrics.max_scroll, 0);
    }

    #[test]
    fn external_controls_and_tiny_areas_do_not_escape_the_reader() {
        let content = ReaderContent {
            numbered: Vec::new(),
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
                    render(frame, frame.area(), &content, None, 0);
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
