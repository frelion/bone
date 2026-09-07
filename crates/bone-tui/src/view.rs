use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
};

use crate::app::{
    App, Focus, ScrollAnchor, SessionState, SessionStatus, SessionUi, Speaker, TimelineItem,
    TimelineKind, Tone, Viewport,
};

const RAIL_WIDTH: u16 = 28;
const RAIL_BREAKPOINT: u16 = 110;
const MESSAGE_INDENT: u16 = 2;

pub(crate) fn render(frame: &mut Frame<'_>, app: &App) -> Viewport {
    let area = frame.area();
    if area.width >= RAIL_BREAKPOINT {
        let [rail, _, conversation] = Layout::horizontal([
            Constraint::Length(RAIL_WIDTH),
            Constraint::Length(2),
            Constraint::Fill(1),
        ])
        .areas(area);
        render_sessions(frame, rail, app, false);
        render_conversation(frame, conversation, app, false)
    } else if app.focus == Focus::Sessions {
        render_sessions(frame, area, app, true);
        Viewport::default()
    } else {
        render_conversation(frame, area, app, true)
    }
}

fn render_sessions(frame: &mut Frame<'_>, area: Rect, app: &App, full_screen: bool) {
    let content = inset(area, 1);
    let [brand, section, sessions, help] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(content);

    frame.render_widget(
        Line::from(vec![
            Span::styled("BONE", accent().add_modifier(Modifier::BOLD)),
            Span::styled(format!("  {}", app.sessions.len()), muted()),
        ]),
        brand,
    );
    frame.render_widget(
        Line::styled("Conversations", muted().add_modifier(Modifier::BOLD)),
        section,
    );

    let visible = usize::from(sessions.height / 2).max(1);
    let start = app
        .current
        .saturating_add(1)
        .saturating_sub(visible)
        .min(app.sessions.len().saturating_sub(visible));
    for (row, (index, session)) in app
        .sessions
        .iter()
        .enumerate()
        .skip(start)
        .take(visible)
        .enumerate()
    {
        let row = Rect::new(sessions.x, sessions.y + row as u16 * 2, sessions.width, 2);
        if index == app.current {
            frame.render_widget(
                Line::styled(
                    "▌",
                    if app.focus == Focus::Sessions {
                        accent().add_modifier(Modifier::BOLD)
                    } else {
                        muted()
                    },
                ),
                Rect::new(row.x, row.y, 1, row.height),
            );
        }

        let title_style = if index == app.current {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        frame.render_widget(
            Line::from(vec![
                Span::styled(format!("{:>2}  ", index + 1), muted()),
                Span::styled(session.title(), title_style),
            ]),
            Rect::new(row.x + 1, row.y, row.width.saturating_sub(2), 1),
        );

        let (marker, label, style) = session_cue(session);
        frame.render_widget(
            Line::from(vec![
                Span::raw("    "),
                Span::styled(marker, style),
                Span::styled(format!(" {label}"), muted()),
            ]),
            Rect::new(row.x + 1, row.y + 1, row.width.saturating_sub(2), 1),
        );
    }

    let help_text = if full_screen {
        "↑↓ select · Enter compose · Ctrl→"
    } else {
        "^N  New conversation"
    };
    frame.render_widget(Line::styled(help_text, muted()), help);
}

fn render_conversation(frame: &mut Frame<'_>, area: Rect, app: &App, narrow: bool) -> Viewport {
    let margin = if area.width >= 48 { 2 } else { 1 };
    let content = inset(area, margin);
    let composer_height = if content.height >= 10 { 4 } else { 3 };
    let header_height = u16::from(narrow);
    let [header, transcript, composer, footer] = Layout::vertical([
        Constraint::Length(header_height),
        Constraint::Fill(1),
        Constraint::Length(composer_height),
        Constraint::Length(1),
    ])
    .areas(content);

    if narrow {
        render_header(frame, header, app);
    }
    let session = app.current();
    let activity = (session.conversation.anchor.is_none())
        .then(|| live_activity(session, content.width < 52))
        .flatten();
    let (timeline, activity_area) = if activity.is_some() && transcript.height > 0 {
        let [timeline, activity] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(transcript);
        (timeline, Some(activity))
    } else {
        (transcript, None)
    };
    render_timeline(
        frame,
        timeline,
        &session.conversation.projection.timeline,
        session.conversation.anchor,
    );
    if let (Some(area), Some((text, tone))) = (activity_area, activity) {
        render_activity(frame, area, text, tone);
    }
    render_composer(
        frame,
        composer,
        &session.conversation.composer,
        app.focus == Focus::Composer,
    );
    render_footer(frame, footer, app, narrow);
    Viewport {
        width: timeline.width,
        height: timeline.height,
        spacing: 0,
    }
}

fn render_header(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let session = app.current();
    let (marker, label, style) = session_cue(session);
    let compact = area.width < 52;
    let mut right = vec![Span::styled(marker, style)];
    if !compact {
        right.extend([Span::raw(" "), Span::styled(label, muted())]);
    }
    if session.conversation.unread {
        right.extend([
            Span::styled("  ↑ ", warning()),
            Span::styled("new", warning()),
        ]);
    }
    let right = Line::from(right).right_aligned();
    let right_width = right.width().min(usize::from(area.width)) as u16;
    let [left_area, _, right_area] = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Length(right_width),
    ])
    .areas(area);

    let mut left = vec![Span::styled("BONE", accent().add_modifier(Modifier::BOLD))];
    left.push(Span::styled(
        format!("  {}/{}", app.current + 1, app.sessions.len()),
        muted(),
    ));
    if let Some((marker, style)) = background_marker(app) {
        left.push(Span::styled(format!(" {marker}"), style));
    }
    left.push(Span::styled("  ·  ", muted()));
    left.push(Span::styled(
        session.title(),
        Style::default().add_modifier(Modifier::BOLD),
    ));
    frame.render_widget(Line::from(left), left_area);
    frame.render_widget(right, right_area);
}

fn live_activity(session: &SessionUi, compact: bool) -> Option<(String, Tone)> {
    match &session.state {
        SessionState::Opening if session.pending_post.is_some() => Some((
            "Message queued · opening conversation".to_owned(),
            Tone::Success,
        )),
        SessionState::Opening => Some(("Opening conversation".to_owned(), Tone::Success)),
        SessionState::Offline(reason) => Some((reason.clone(), Tone::Error)),
        SessionState::Live if compact && session.conversation.projection.active.len() > 1 => {
            Some((
                format!("{} active", session.conversation.projection.active.len()),
                Tone::Success,
            ))
        }
        SessionState::Live => session
            .conversation
            .projection
            .activity()
            .map(|activity| (activity, Tone::Success)),
    }
}

fn render_timeline(
    frame: &mut Frame<'_>,
    area: Rect,
    items: &[TimelineItem],
    anchor: Option<ScrollAnchor>,
) {
    if items.is_empty() {
        render_empty_state(frame, area);
        return;
    }
    if area.width <= MESSAGE_INDENT || area.height == 0 {
        return;
    }
    let (start, first_offset, top_padding) = anchor.map_or_else(
        || tail_start(items, area.width, usize::from(area.height)),
        |anchor| {
            let index = items
                .iter()
                .position(|item| item.cursor == anchor.cursor)
                .unwrap_or_default();
            let height = items[index].line_count(area.width);
            (index, anchor.line.min(height.saturating_sub(1)), 0)
        },
    );

    let mut screen_row = top_padding;
    for (relative, item) in items[start..].iter().enumerate() {
        if screen_row >= usize::from(area.height) {
            break;
        }
        let height = item.line_count(area.width);
        let offset = if relative == 0 { first_offset } else { 0 };
        let visible = (height - offset).min(usize::from(area.height) - screen_row);
        let row = Rect::new(
            area.x,
            area.y + screen_row as u16,
            area.width,
            visible as u16,
        );
        render_item(frame, row, item, offset as u16);
        screen_row += visible;
        if screen_row >= usize::from(area.height) {
            break;
        }
    }
}

fn render_activity(frame: &mut Frame<'_>, area: Rect, text: String, tone: Tone) {
    let item = TimelineItem {
        cursor: u64::MAX,
        kind: TimelineKind::Status {
            text: format!("• {text}"),
            tone,
        },
        attention: false,
    };
    render_item(frame, area, &item, 0);
}

fn render_empty_state(frame: &mut Frame<'_>, area: Rect) {
    if area.height < 2 {
        return;
    }
    let y = area.y + area.height.saturating_sub(2) / 3;
    frame.render_widget(
        Line::styled(
            "What should we work on?",
            Style::default().add_modifier(Modifier::BOLD),
        )
        .centered(),
        Rect::new(area.x, y, area.width, 1),
    );
    frame.render_widget(
        Line::styled("Ask a question or describe a task.", muted()).centered(),
        Rect::new(area.x, y + 1, area.width, 1),
    );
}

fn tail_start(items: &[TimelineItem], width: u16, height: usize) -> (usize, usize, usize) {
    let mut used = 0;
    for index in (0..items.len()).rev() {
        let item_height = items[index].line_count(width);
        let available = height - used;
        if item_height >= available {
            return (index, item_height - available, 0);
        }
        used += item_height;
        if index == 0 {
            return (index, 0, 0);
        }
    }
    (0, 0, 0)
}

fn render_item(frame: &mut Frame<'_>, area: Rect, item: &TimelineItem, offset: u16) {
    let text = Rect::new(
        area.x + MESSAGE_INDENT,
        area.y,
        area.width.saturating_sub(MESSAGE_INDENT),
        area.height,
    );
    match &item.kind {
        TimelineKind::Message {
            speaker: Speaker::User,
            ..
        } => {
            if offset == 0 {
                frame.render_widget(
                    Line::styled("┃", accent()),
                    Rect::new(area.x, area.y, 1, area.height),
                );
            }
        }
        TimelineKind::Message {
            speaker: Speaker::Bone,
            ..
        } => {
            if offset == 0 {
                frame.render_widget(Line::styled("●", accent()), Rect::new(area.x, area.y, 1, 1));
            }
        }
        TimelineKind::Error(_) if offset == 0 => frame.render_widget(
            Line::styled("!", error().add_modifier(Modifier::BOLD)),
            Rect::new(area.x, area.y, 1, 1),
        ),
        TimelineKind::Tool { .. } | TimelineKind::Status { .. } | TimelineKind::Error(_) => {}
    }
    frame.render_widget(item_paragraph(item).scroll((offset, 0)), text);
}

fn item_paragraph(item: &TimelineItem) -> Paragraph<'_> {
    let paragraph = match &item.kind {
        TimelineKind::Message { text, .. } => Paragraph::new(text.as_str()),
        TimelineKind::Error(text) => Paragraph::new(text.as_str()).style(error()),
        TimelineKind::Tool { text, tone } | TimelineKind::Status { text, tone } => {
            let split = text.chars().next().map_or(0, char::len_utf8);
            let (marker, rest) = text.split_at(split);
            Paragraph::new(Line::from(vec![
                Span::styled(marker, tone_style(*tone)),
                Span::styled(rest, muted()),
            ]))
        }
    };
    paragraph.wrap(Wrap { trim: false })
}

fn render_composer(
    frame: &mut Frame<'_>,
    area: Rect,
    composer: &ratatui_textarea::TextArea<'static>,
    focused: bool,
) {
    frame.render_widget(
        Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(if focused { accent() } else { composer_border() }),
        area,
    );
    let inner = Rect::new(
        area.x + 2,
        area.y + 1,
        area.width.saturating_sub(4),
        area.height.saturating_sub(2),
    );
    if focused {
        frame.render_widget(composer, inner);
    } else {
        let draft = composer.lines().join("\n");
        let text = if draft.is_empty() {
            Paragraph::new("Ask BONE…").style(muted())
        } else {
            Paragraph::new(draft)
        };
        frame.render_widget(text.wrap(Wrap { trim: false }), inner);
    }
}

fn render_footer(frame: &mut Frame<'_>, area: Rect, app: &App, narrow: bool) {
    let session = app.current();
    let compact = narrow && area.width < 60;
    let help = if app.focus == Focus::Sessions {
        "↑↓ select · Enter compose · Ctrl→".to_owned()
    } else if session.conversation.anchor.is_some() {
        if compact && session.conversation.unread {
            "PgDn · ↑ new · Ctrl← sessions".to_owned()
        } else if compact {
            "PgDn latest · Ctrl← sessions".to_owned()
        } else if session.conversation.unread {
            "PgDn latest · ↑ new · Ctrl← sessions".to_owned()
        } else {
            "PgDn latest · Ctrl← sessions".to_owned()
        }
    } else {
        match (&session.state, compact) {
            (SessionState::Opening, true) if session.pending_post.is_some() => {
                "Queued · Ctrl← sessions"
            }
            (SessionState::Opening, false) if session.pending_post.is_some() => {
                "Queued while opening · Ctrl← sessions"
            }
            (SessionState::Opening, true) => "Enter queue · Ctrl← sessions",
            (SessionState::Opening, false) => "Enter queue · ^N new · Ctrl← sessions",
            (SessionState::Offline(_), true) => "^N new · Ctrl← sessions",
            (SessionState::Offline(_), false) => "Draft kept · ^N new · Ctrl← sessions",
            (SessionState::Live, _) if !session.conversation.projection.active.is_empty() => {
                "Esc stop · Ctrl← sessions"
            }
            (SessionState::Live, true) => "^N new · Ctrl← sessions",
            (SessionState::Live, false) => "Enter send · ^N new · Ctrl← sessions",
        }
        .to_owned()
    };
    let right = Line::styled(help, muted()).right_aligned();
    let right_width = right.width().min(usize::from(area.width)) as u16;
    if area.width < 60 {
        frame.render_widget(right, area);
        return;
    }
    let [workspace, _, help] = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Length(right_width),
    ])
    .areas(area);
    frame.render_widget(
        Line::styled(compact_workspace(&app.workspace), muted()),
        workspace,
    );
    frame.render_widget(right, help);
}

fn session_cue(session: &SessionUi) -> (&'static str, String, Style) {
    if session.conversation.projection.has_unknown_effect() {
        return ("!", "Unresolved effect".to_owned(), warning());
    }
    if let SessionState::Offline(_) = &session.state {
        return ("!", "Offline".to_owned(), error());
    }
    if session.background_unread {
        return ("●", "New activity".to_owned(), warning());
    }
    match &session.state {
        SessionState::Opening if session.pending_post.is_some() => {
            ("•", "Message queued".to_owned(), accent())
        }
        SessionState::Opening => ("•", "Opening".to_owned(), accent()),
        SessionState::Offline(_) => unreachable!(),
        SessionState::Live => match session.conversation.projection.status {
            SessionStatus::Working => {
                let jobs = session.conversation.projection.active.len();
                if jobs > 1 {
                    ("•", format!("{jobs} jobs running"), accent())
                } else {
                    ("•", "Working".to_owned(), accent())
                }
            }
            SessionStatus::Waiting => ("·", "Waiting".to_owned(), warning()),
            SessionStatus::Stopped => ("·", "Stopped".to_owned(), warning()),
            SessionStatus::Complete => ("✓", "Complete".to_owned(), success()),
            SessionStatus::Ready => ("·", "Ready".to_owned(), muted()),
        },
    }
}

fn background_marker(app: &App) -> Option<(&'static str, Style)> {
    let background = app
        .sessions
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != app.current)
        .map(|(_, session)| session);
    if background.clone().any(|session| {
        matches!(&session.state, SessionState::Offline(_))
            || session.conversation.projection.has_unknown_effect()
    }) {
        return Some(("!", warning()));
    }
    if background.clone().any(|session| session.background_unread) {
        return Some(("●", warning()));
    }
    background
        .clone()
        .any(|session| session.state == SessionState::Opening)
        .then(|| ("…", accent()))
}

fn compact_workspace(workspace: &str) -> String {
    let components = workspace
        .trim_end_matches('/')
        .split('/')
        .filter(|component| !component.is_empty())
        .collect::<Vec<_>>();
    if components.len() <= 2 || workspace.starts_with('~') {
        workspace.to_owned()
    } else {
        format!(
            "…/{}/{}",
            components[components.len() - 2],
            components[components.len() - 1]
        )
    }
}

fn inset(area: Rect, horizontal: u16) -> Rect {
    Rect::new(
        area.x + horizontal.min(area.width),
        area.y,
        area.width.saturating_sub(horizontal.saturating_mul(2)),
        area.height,
    )
}

fn tone_style(tone: Tone) -> Style {
    match tone {
        Tone::Success => muted(),
        Tone::Warning => warning(),
        Tone::Error => error(),
    }
}

fn accent() -> Style {
    Style::default().fg(Color::Rgb(96, 165, 250))
}

fn muted() -> Style {
    Style::default().fg(Color::Rgb(112, 122, 138))
}

fn success() -> Style {
    Style::default().fg(Color::Rgb(74, 222, 128))
}

fn warning() -> Style {
    Style::default().fg(Color::Rgb(251, 191, 36))
}

fn error() -> Style {
    Style::default().fg(Color::Rgb(248, 113, 113))
}

fn composer_border() -> Style {
    Style::default().fg(Color::Rgb(75, 86, 104))
}

#[cfg(test)]
mod tests {
    use bone_agent::{
        JobId, JobOutcome, JobRequest, Message, MessageId, Notice, RecordEntry, RecordKind,
        Snapshot, ToolCall,
    };
    use ratatui::{
        Terminal,
        backend::TestBackend,
        buffer::{Buffer, CellWidth},
        style::Color,
    };
    use serde_json::json;

    use super::render;
    use crate::app::{App, Focus, SessionId, Viewport};

    #[test]
    fn wide_layout_has_a_session_rail_and_semantic_live_tail() {
        let mut app = example_app();
        app.sessions[1]
            .conversation
            .composer
            .insert_str("继续打磨交互和代码结构");

        let buffer = draw(&app, 120, 28);
        let screen = screen(&buffer);
        assert!(row(&buffer, 0).starts_with(" BONE  2"));
        assert!(screen.contains("Conversations"));
        assert!(screen.contains("▌ 2  实现多 session"));
        assert!(screen.contains("┃ 实现多 session TUI"));
        assert!(screen.contains("● 已经建立独立 session"));
        assert!(screen.contains("✓ Searched \"Projection\" in crates · 3 matches"));
        assert!(screen.contains("• 2 active · Thinking · Reading crates/bone-tui/src/view.rs 68%"));
        assert!(screen.contains("继续打磨交互和代码结构"));
        assert!(screen.contains("Esc stop · Ctrl← sessions"));
        assert_eq!(buffer[(32, 0)].symbol(), "┃");
        assert_eq!(buffer[(27, 0)].fg, Color::Reset);
        assert_eq!(buffer[(1, 0)].bg, Color::Reset);
    }

    #[test]
    fn ordinary_terminal_width_gives_the_conversation_the_full_screen() {
        let app = example_app();
        let buffer = draw(&app, 80, 24);
        let screen = screen(&buffer);

        assert!(row(&buffer, 0).starts_with("  BONE  2/2 ●"));
        assert!(!screen.contains("Conversations"));
        assert!(screen.contains("┃ 实现多 session TUI"));
        assert!(screen.contains("● 已经建立独立 session"));
        assert!(screen.contains("╭"));
        assert!(screen.contains("╯"));
    }

    #[test]
    fn compact_layout_keeps_one_header_one_footer_and_a_real_composer() {
        let mut app = example_app();
        app.sessions[1]
            .conversation
            .composer
            .insert_str("继续比较另外两个方案");

        let buffer = draw(&app, 40, 12);
        let screen = screen(&buffer);
        assert!(row(&buffer, 0).starts_with(" BONE  2/2 ●"));
        assert!(!screen.contains("Conversations"));
        assert!(screen.contains("• 2 active"));
        assert!(screen.contains("继续比较另外两个方案"));
        assert!(row(&buffer, 11).contains("Esc stop"));
        assert_eq!(buffer[(1, 7)].symbol(), "╭");
    }

    #[test]
    fn compact_session_focus_uses_a_full_screen_list() {
        let mut app = example_app();
        app.focus = Focus::Sessions;

        let buffer = draw(&app, 40, 12);
        let screen = screen(&buffer);
        assert!(row(&buffer, 0).starts_with(" BONE  2"));
        assert!(screen.contains("Conversations"));
        assert!(screen.contains("▌ 2  实现多 session"));
        assert!(row(&buffer, 11).contains("↑↓ select · Enter compose · Ctrl→"));
        assert!(!screen.contains("Ask BONE"));
    }

    #[test]
    fn live_activity_uses_one_row_outside_the_scrollable_timeline() {
        let app = example_app();
        let (_, viewport) = draw_with_viewport(&app, 40, 12);

        assert_eq!(
            viewport,
            Viewport {
                width: 38,
                height: 5,
                spacing: 0,
            }
        );
    }

    #[test]
    fn unresolved_effect_outranks_background_unread() {
        let mut app = App::new("workspace".into());
        app.add_session(
            SessionId(1),
            &snapshot(vec![
                RecordEntry {
                    cursor: 1,
                    kind: RecordKind::Notice(Notice::JobStarted {
                        id: JobId(1),
                        request: JobRequest::Tool(ToolCall::new(
                            "apply_patch",
                            json!({"patch": "..."}),
                        )),
                    }),
                },
                RecordEntry {
                    cursor: 2,
                    kind: RecordKind::Notice(Notice::JobFinished {
                        id: JobId(1),
                        outcome: JobOutcome::unknown("connection lost"),
                    }),
                },
            ]),
            false,
        );
        app.sessions[0].background_unread = true;
        app.mark_offline(SessionId(1), "agent runtime closed");

        let screen = screen(&draw(&app, 120, 20));
        assert!(screen.contains("! Unresolved effect"));
        assert!(!screen.contains("● New activity"));
        assert!(!screen.contains("! Offline"));
    }

    #[test]
    fn empty_session_has_a_calm_starting_point() {
        let mut app = App::new("/work/BONE".into());
        app.add_session(SessionId(1), &snapshot(vec![]), true);
        let screen = screen(&draw(&app, 80, 18));

        assert!(screen.contains("What should we work on?"));
        assert!(screen.contains("Ask a question or describe a task."));
        assert!(screen.contains("Ask BONE"));
    }

    fn example_app() -> App {
        let mut app = App::new("/Users/zzhang/Documents/ChatGPT/BONE".into());
        app.add_session(
            SessionId(1),
            &snapshot(vec![RecordEntry {
                cursor: 1,
                kind: RecordKind::UserMessage(Message {
                    id: MessageId(1),
                    text: "重构事件驱动 Agent".into(),
                }),
            }]),
            true,
        );
        app.sessions[0].background_unread = true;

        app.add_session(
            SessionId(2),
            &snapshot(vec![
                RecordEntry {
                    cursor: 1,
                    kind: RecordKind::UserMessage(Message {
                        id: MessageId(2),
                        text: "实现多 session TUI\n侧栏需要随时切换对话。".into(),
                    }),
                },
                RecordEntry {
                    cursor: 2,
                    kind: RecordKind::Notice(Notice::Reply {
                        text: "已经建立独立 session；历史、草稿和运行状态彼此隔离。".into(),
                        reply_to: vec![MessageId(2)],
                        as_of: 1,
                    }),
                },
                RecordEntry {
                    cursor: 3,
                    kind: RecordKind::Notice(Notice::JobStarted {
                        id: JobId(1),
                        request: JobRequest::Tool(ToolCall::new(
                            "grep",
                            json!({"pattern": "Projection", "path": "crates"}),
                        )),
                    }),
                },
                RecordEntry {
                    cursor: 4,
                    kind: RecordKind::Notice(Notice::JobFinished {
                        id: JobId(1),
                        outcome: JobOutcome::artifact(json!({"match_count": 3})),
                    }),
                },
                RecordEntry {
                    cursor: 5,
                    kind: RecordKind::Notice(Notice::JobStarted {
                        id: JobId(2),
                        request: JobRequest::Work { messages: vec![] },
                    }),
                },
                RecordEntry {
                    cursor: 6,
                    kind: RecordKind::Notice(Notice::JobStarted {
                        id: JobId(3),
                        request: JobRequest::Tool(ToolCall::new(
                            "read",
                            json!({"path": "crates/bone-tui/src/view.rs"}),
                        )),
                    }),
                },
                RecordEntry {
                    cursor: 7,
                    kind: RecordKind::Notice(Notice::JobProgress {
                        id: JobId(3),
                        progress: bone_agent::JobProgress {
                            message: String::new(),
                            percent: Some(68),
                        },
                    }),
                },
            ]),
            true,
        );
        app
    }

    fn snapshot(record: Vec<RecordEntry>) -> Snapshot {
        Snapshot {
            record_cursor: record.last().map_or(0, |entry| entry.cursor),
            revision: 0,
            generation: 0,
            requirement: None,
            autonomous: false,
            work: None,
            review: None,
            candidate: None,
            pending_messages: Vec::new(),
            jobs: Vec::new(),
            record,
            tools: Vec::new(),
        }
    }

    fn draw(app: &App, width: u16, height: u16) -> Buffer {
        draw_with_viewport(app, width, height).0
    }

    fn draw_with_viewport(app: &App, width: u16, height: u16) -> (Buffer, Viewport) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let mut viewport = Viewport::default();
        terminal
            .draw(|frame| {
                viewport = render(frame, app);
            })
            .unwrap();
        (terminal.backend().buffer().clone(), viewport)
    }

    fn screen(buffer: &Buffer) -> String {
        (0..buffer.area.height)
            .map(|y| row(buffer, y))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn row(buffer: &Buffer, y: u16) -> String {
        let mut row = String::new();
        let mut x = 0;
        while x < buffer.area.width {
            let symbol = buffer[(x, y)].symbol();
            row.push_str(symbol);
            x += symbol.cell_width().max(1);
        }
        row.trim_end().to_owned()
    }
}
