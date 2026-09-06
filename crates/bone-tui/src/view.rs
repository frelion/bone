use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
};

use crate::app::{
    App, ScrollAnchor, SessionState, SessionStatus, Speaker, TimelineItem, TimelineKind, Tone,
    Viewport,
};

const RAIL_WIDTH: u16 = 22;
const RAIL_BREAKPOINT: u16 = 72;
const GUTTER_WIDTH: u16 = 6;

pub(crate) fn render(frame: &mut Frame<'_>, app: &App) -> Viewport {
    let area = frame.area();
    if area.width >= RAIL_BREAKPOINT {
        let [rail, divider, conversation] = Layout::horizontal([
            Constraint::Length(RAIL_WIDTH),
            Constraint::Length(1),
            Constraint::Fill(1),
        ])
        .areas(area);
        render_sessions(frame, rail, app);
        render_divider(frame, divider);
        render_conversation(frame, conversation, app, false)
    } else {
        render_conversation(frame, area, app, true)
    }
}

fn render_sessions(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let [title, sessions, help] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(area);

    frame.render_widget(
        Line::from(vec![
            Span::styled("SESSIONS", muted().add_modifier(Modifier::BOLD)),
            Span::styled(
                format!(" {}/{}", app.current + 1, app.sessions.len()),
                muted(),
            ),
        ]),
        title,
    );

    let visible = usize::from(sessions.height);
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
        let row = Rect::new(sessions.x, sessions.y + row as u16, sessions.width, 1);
        let marker = if index == app.current { "▌" } else { " " };
        let (badge, badge_style) = session_badge(session);
        let badge_width = Line::from(badge.as_str()).width() as u16;
        let [marker_area, name_area, badge_area] = Layout::horizontal([
            Constraint::Length(2),
            Constraint::Fill(1),
            Constraint::Length(badge_width),
        ])
        .areas(row);

        frame.render_widget(Line::styled(marker, accent()), marker_area);
        let name_style = if index == app.current {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        frame.render_widget(Line::styled(session.title(), name_style), name_area);
        frame.render_widget(Line::styled(badge, badge_style).right_aligned(), badge_area);
    }

    frame.render_widget(Line::styled("^N new  Alt↑↓ switch", muted()), help);
}

fn session_badge(session: &crate::app::SessionUi) -> (String, Style) {
    match &session.state {
        SessionState::Opening => return ("…".into(), accent()),
        SessionState::Offline(_) => return ("!".into(), error()),
        SessionState::Live => {}
    }
    if session.background_unread {
        return ("*".into(), warning());
    }
    if session.conversation.projection.has_unknown_effect() {
        return ("!".into(), warning());
    }
    match session.conversation.projection.status {
        SessionStatus::Working => ("•".into(), accent()),
        SessionStatus::Waiting => ("?".into(), warning()),
        SessionStatus::Stopped => ("–".into(), warning()),
        SessionStatus::Complete => ("✓".into(), success()),
        SessionStatus::Ready => (String::new(), Style::default()),
    }
}

fn render_divider(frame: &mut Frame<'_>, area: Rect) {
    let lines = (0..area.height)
        .map(|_| Line::styled("│", muted()))
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(lines), area);
}

fn render_conversation(frame: &mut Frame<'_>, area: Rect, app: &App, narrow: bool) -> Viewport {
    let session = app.current();
    let conversation = &session.conversation;
    let composer_height = if area.height < 16 { 2 } else { 3 };
    let spacer_height = u16::from(area.height >= 16);
    let footer_height = if narrow { 2 } else { 1 };
    let [header, _, transcript, activity, rule, composer, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(spacer_height),
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(composer_height),
        Constraint::Length(footer_height),
    ])
    .areas(area);

    render_header(frame, header, app, narrow);
    render_timeline(
        frame,
        transcript,
        &conversation.projection.timeline,
        conversation.anchor,
        area.height >= 16,
    );
    render_activity(frame, activity, app);
    frame.render_widget(
        Line::styled("─".repeat(usize::from(rule.width)), muted()),
        rule,
    );
    render_composer(frame, composer, &conversation.composer);
    render_help(frame, footer, narrow, &session.state);
    Viewport {
        width: transcript.width.saturating_sub(GUTTER_WIDTH),
        height: transcript.height,
        spacing: usize::from(area.height >= 16),
    }
}

fn render_header(frame: &mut Frame<'_>, area: Rect, app: &App, narrow: bool) {
    let session = app.current();
    let mut right = match &session.state {
        SessionState::Opening => vec![Span::styled("opening…", accent())],
        SessionState::Offline(_) => vec![Span::styled("offline", error())],
        SessionState::Live if session.conversation.projection.has_unknown_effect() => {
            vec![Span::styled("! unresolved", warning())]
        }
        SessionState::Live => vec![Span::styled(
            session.conversation.projection.status.label(),
            status_style(session.conversation.projection.status),
        )],
    };
    if session.conversation.unread {
        right.extend([
            Span::styled(" · ", muted()),
            Span::styled("↑ new", warning()),
        ]);
    }
    let right = Line::from(right).right_aligned();
    let right_width = right.width().min(usize::from(area.width)) as u16;
    let [left_area, right_area] =
        Layout::horizontal([Constraint::Fill(1), Constraint::Length(right_width)]).areas(area);

    let mut left = vec![Span::styled("BONE", accent().add_modifier(Modifier::BOLD))];
    if narrow {
        left.push(Span::styled(
            format!("  {}/{}", app.current + 1, app.sessions.len()),
            muted(),
        ));
        if let Some((marker, style)) = background_marker(app) {
            left.push(Span::styled(format!(" {marker}"), style));
        }
        left.push(Span::styled(" · ", muted()));
        left.push(Span::raw(session.title()));
    } else {
        left.push(Span::styled(format!("  {}", app.workspace), muted()));
    }
    frame.render_widget(Line::from(left), left_area);
    frame.render_widget(right, right_area);
}

fn background_marker(app: &App) -> Option<(&'static str, Style)> {
    let mut unread = false;
    let mut issue = false;
    let mut opening = false;
    for (index, session) in app.sessions.iter().enumerate() {
        if index == app.current {
            continue;
        }
        unread |= session.background_unread;
        issue |= matches!(&session.state, SessionState::Offline(_))
            || session.conversation.projection.has_unknown_effect();
        opening |= session.state == SessionState::Opening;
    }
    if unread {
        return Some(("*", warning()));
    }
    if issue {
        return Some(("!", warning()));
    }
    opening.then(|| ("…", accent()))
}

fn render_timeline(
    frame: &mut Frame<'_>,
    area: Rect,
    items: &[TimelineItem],
    anchor: Option<ScrollAnchor>,
    roomy: bool,
) {
    if items.is_empty() {
        let body = body_area(area);
        frame.render_widget(
            Paragraph::new("Describe a task below. Each conversation keeps its own draft.")
                .style(muted())
                .wrap(Wrap { trim: false }),
            body,
        );
        return;
    }

    let body_width = area.width.saturating_sub(GUTTER_WIDTH);
    if body_width == 0 || area.height == 0 {
        return;
    }
    let spacing = usize::from(roomy);
    let (start, first_offset, top_padding) = anchor.map_or_else(
        || tail_start(items, body_width, usize::from(area.height), spacing),
        |anchor| {
            let index = items
                .iter()
                .position(|item| item.cursor == anchor.cursor)
                .unwrap_or_default();
            let height = items[index].line_count(body_width);
            (index, anchor.line.min(height.saturating_sub(1)), 0)
        },
    );

    let mut screen_row = top_padding;
    for (relative, item) in items[start..].iter().enumerate() {
        let height = item.line_count(body_width);
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
        screen_row = (screen_row + spacing).min(usize::from(area.height));
    }
}

fn tail_start(
    items: &[TimelineItem],
    width: u16,
    height: usize,
    spacing: usize,
) -> (usize, usize, usize) {
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
        if used + spacing >= height {
            return (index, 0, height - used);
        }
        used += spacing;
    }
    (0, 0, 0)
}

fn render_item(frame: &mut Frame<'_>, area: Rect, item: &TimelineItem, offset: u16) {
    if offset == 0
        && let Some((label, style)) = item_label(item)
    {
        frame.render_widget(
            Line::styled(label, style),
            Rect::new(area.x, area.y, GUTTER_WIDTH, 1),
        );
    }
    frame.render_widget(item_paragraph(item).scroll((offset, 0)), body_area(area));
}

fn item_label(item: &TimelineItem) -> Option<(&'static str, Style)> {
    match &item.kind {
        TimelineKind::Message {
            speaker: Speaker::User,
            ..
        } => Some(("you", Style::default().add_modifier(Modifier::BOLD))),
        TimelineKind::Message {
            speaker: Speaker::Bone,
            ..
        } => Some(("bone", accent().add_modifier(Modifier::BOLD))),
        TimelineKind::Error(_) => Some(("error", error().add_modifier(Modifier::BOLD))),
        TimelineKind::Tool { .. } | TimelineKind::Status { .. } => None,
    }
}

fn item_paragraph(item: &TimelineItem) -> Paragraph<'_> {
    let paragraph = match &item.kind {
        TimelineKind::Message { text, .. } | TimelineKind::Error(text) => {
            Paragraph::new(text.as_str())
        }
        TimelineKind::Tool { text, tone } | TimelineKind::Status { text, tone } => {
            let split = text.chars().next().map_or(0, char::len_utf8);
            let (marker, rest) = text.split_at(split);
            Paragraph::new(Line::from(vec![
                Span::styled(marker, tone_style(*tone)),
                Span::raw(rest),
            ]))
        }
    };
    paragraph.wrap(Wrap { trim: false })
}

fn render_activity(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let session = app.current();
    let (marker, marker_style, activity) = match &session.state {
        SessionState::Opening => ("•", accent(), "opening conversation".to_owned()),
        SessionState::Offline(reason) => ("×", error(), reason.clone()),
        SessionState::Live => {
            let projection = &session.conversation.projection;
            if projection.active.is_empty() {
                return;
            }
            let Some(activity) = projection.activity() else {
                return;
            };
            ("•", accent(), activity)
        }
    };
    frame.render_widget(
        Line::from(vec![
            Span::styled(marker, marker_style),
            Span::raw(" "),
            Span::raw(activity),
        ]),
        area,
    );
}

fn render_composer(
    frame: &mut Frame<'_>,
    area: Rect,
    composer: &ratatui_textarea::TextArea<'static>,
) {
    frame.render_widget(
        Line::styled("›", accent().add_modifier(Modifier::BOLD)),
        Rect::new(area.x, area.y, 2, 1),
    );
    frame.render_widget(
        composer,
        Rect::new(
            area.x + 2,
            area.y,
            area.width.saturating_sub(2),
            area.height,
        ),
    );
}

fn render_help(frame: &mut Frame<'_>, area: Rect, narrow: bool, state: &SessionState) {
    let conversation_help = match state {
        SessionState::Opening => "Type now  Enter when ready  ^C exit",
        SessionState::Offline(_) => "Draft kept  ^N new  ^C exit",
        SessionState::Live => "Enter send  ^J line  Esc stop  ^C exit",
    };
    if narrow {
        let [sessions, conversation] =
            Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(area);
        frame.render_widget(Line::styled("^N new  Alt↑↓ sessions", muted()), sessions);
        frame.render_widget(Line::styled(conversation_help, muted()), conversation);
    } else {
        let help = match state {
            SessionState::Opening => "Type now  Enter when ready  Ctrl-C exit",
            SessionState::Offline(_) => "Draft kept  Ctrl-N new  Ctrl-C exit",
            SessionState::Live => "Enter send  ^J line  PgUp/Dn  Esc stop  ^C exit",
        };
        frame.render_widget(Line::styled(help, muted()), area);
    }
}

fn body_area(area: Rect) -> Rect {
    Rect::new(
        area.x + GUTTER_WIDTH,
        area.y,
        area.width.saturating_sub(GUTTER_WIDTH),
        area.height,
    )
}

fn status_style(status: SessionStatus) -> Style {
    match status {
        SessionStatus::Working => accent(),
        SessionStatus::Waiting | SessionStatus::Stopped => warning(),
        SessionStatus::Complete => success(),
        SessionStatus::Ready => muted(),
    }
}

fn tone_style(tone: Tone) -> Style {
    match tone {
        Tone::Success => success(),
        Tone::Warning => warning(),
        Tone::Error => error(),
    }
}

fn accent() -> Style {
    Style::default().fg(Color::Cyan)
}

fn muted() -> Style {
    Style::default().fg(Color::DarkGray)
}

fn success() -> Style {
    Style::default().fg(Color::Green)
}

fn warning() -> Style {
    Style::default().fg(Color::Yellow)
}

fn error() -> Style {
    Style::default().fg(Color::Red)
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
    use crate::app::{App, SessionId};

    #[test]
    fn wide_layout_has_session_rail_and_a_quiet_conversation_surface() {
        let mut app = example_app();
        app.sessions[1]
            .conversation
            .composer
            .insert_str("完成后把取舍写清楚。");

        let buffer = draw(&app, 80, 24);
        let screen = screen(&buffer);

        assert!(row(&buffer, 0).starts_with("SESSIONS"));
        assert_eq!(buffer[(22, 0)].symbol(), "│");
        assert_eq!(buffer[(23, 0)].symbol(), "B");
        assert!(screen.contains("▌"));
        assert!(screen.contains("当前会话"));
        assert!(screen.contains("you   当前会话"));
        assert!(screen.contains("      不要继续 A"));
        assert!(screen.contains("bone  明白"));
        assert!(screen.contains("✓ grep"));
        assert!(screen.contains("• 2 active · thinking · read 42%"));
        assert!(row(&buffer, 20).contains("› 完成后把取舍写清楚。"));
        assert!(row(&buffer, 23).starts_with("^N new"));
        assert_eq!(buffer[(23, 0)].fg, Color::Cyan);
        assert_eq!(buffer[(23, 0)].bg, Color::Reset);
    }

    #[test]
    fn narrow_layout_replaces_the_rail_with_session_context() {
        let mut app = example_app();
        app.sessions[1]
            .conversation
            .composer
            .insert_str("继续比较另外两个方案");

        let buffer = draw(&app, 40, 12);
        let header = row(&buffer, 0);
        let screen = screen(&buffer);

        assert!(header.starts_with("BONE  2/2 * · 当前会话"));
        assert!(!screen.contains("SESSIONS"));
        assert!(screen.contains("you   当前会话"));
        assert!(screen.contains("      不要继续 A"));
        assert!(screen.contains("bone  明白"));
        assert!(row(&buffer, 8).contains("› 继续比较另外两个方案"));
        assert_eq!(row(&buffer, 10), "^N new  Alt↑↓ sessions");
        assert_eq!(row(&buffer, 11), "Enter send  ^J line  Esc stop  ^C exit");
    }

    #[test]
    fn opening_and_offline_states_belong_to_their_placeholder() {
        let mut app = App::new("workspace".into());
        app.add_session(SessionId(1), &snapshot(vec![]), true);
        app.begin_session(SessionId(2), true);
        app.sessions[1]
            .conversation
            .composer
            .insert_str("keep this draft");

        let opening = screen(&draw(&app, 40, 12));
        assert!(opening.contains("opening…"));
        assert!(opening.contains("opening conversation"));
        assert!(opening.contains("› keep this draft"));

        app.mark_offline(SessionId(2), "model unavailable");
        let offline = screen(&draw(&app, 40, 12));
        assert!(offline.contains("offline"));
        assert!(offline.contains("× model unavailable"));
        assert!(offline.contains("› keep this draft"));
    }

    #[test]
    fn narrow_header_keeps_unknown_external_effects_visible() {
        let mut app = App::new("workspace".into());
        app.add_session(
            SessionId(1),
            &snapshot(vec![
                RecordEntry {
                    cursor: 1,
                    kind: RecordKind::Notice(Notice::JobStarted {
                        id: JobId(1),
                        request: JobRequest::Tool(ToolCall::new("write", json!({}))),
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

        assert!(row(&draw(&app, 40, 12), 0).contains("! unresolved"));
    }

    fn example_app() -> App {
        let mut app = App::new("~/Documents/ChatGPT/BONE".into());
        app.add_session(
            SessionId(1),
            &snapshot(vec![RecordEntry {
                cursor: 1,
                kind: RecordKind::UserMessage(Message {
                    id: MessageId(1),
                    text: "后台会话".into(),
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
                        text: "当前会话\n不要继续 A，比较 B 和 C。".into(),
                    }),
                },
                RecordEntry {
                    cursor: 2,
                    kind: RecordKind::Notice(Notice::Reply {
                        text: "明白。旧建议返回后也不会直接执行。".into(),
                        reply_to: vec![MessageId(2)],
                        as_of: 1,
                    }),
                },
                RecordEntry {
                    cursor: 3,
                    kind: RecordKind::Notice(Notice::JobStarted {
                        id: JobId(1),
                        request: JobRequest::Tool(ToolCall::new("grep", json!({}))),
                    }),
                },
                RecordEntry {
                    cursor: 4,
                    kind: RecordKind::Notice(Notice::JobFinished {
                        id: JobId(1),
                        outcome: JobOutcome::artifact(json!({"matches": 3})),
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
                        request: JobRequest::Tool(ToolCall::new("read", json!({}))),
                    }),
                },
                RecordEntry {
                    cursor: 7,
                    kind: RecordKind::Notice(Notice::JobProgress {
                        id: JobId(3),
                        progress: bone_agent::JobProgress {
                            message: "reading".into(),
                            percent: Some(42),
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
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| {
                render(frame, app);
            })
            .unwrap();
        terminal.backend().buffer().clone()
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
