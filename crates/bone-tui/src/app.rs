use std::collections::{BTreeMap, BTreeSet};

use bone_agent::{
    ExternalEffect, JobErrorKind, JobId, JobOutput, JobProgress, JobRequest, Notice, RecordEntry,
    RecordKind, Snapshot, StepEvent, ToolCall,
};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    style::Style,
    widgets::{Paragraph, Wrap},
};
use ratatui_textarea::{TextArea, WrapMode};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SessionId(pub(crate) u64);

pub(crate) struct App {
    pub(crate) sessions: Vec<SessionUi>,
    pub(crate) current: usize,
    pub(crate) workspace: String,
    pub(crate) focus: Focus,
    viewport: Viewport,
}

impl App {
    pub(crate) fn new(workspace: String) -> Self {
        Self {
            sessions: Vec::new(),
            current: 0,
            workspace,
            focus: Focus::Composer,
            viewport: Viewport::default(),
        }
    }

    pub(crate) fn add_session(&mut self, id: SessionId, snapshot: &Snapshot, show_progress: bool) {
        self.sessions.push(SessionUi {
            id,
            conversation: Conversation::new(snapshot, show_progress),
            background_unread: false,
            state: SessionState::Live,
            pending_post: None,
        });
        self.current = self.sessions.len() - 1;
        self.focus = Focus::Composer;
    }

    pub(crate) fn begin_session(&mut self, id: SessionId, show_progress: bool) {
        self.sessions.push(SessionUi {
            id,
            conversation: Conversation::empty(show_progress),
            background_unread: false,
            state: SessionState::Opening,
            pending_post: None,
        });
        self.current = self.sessions.len() - 1;
        self.focus = Focus::Composer;
    }

    pub(crate) fn attach(&mut self, id: SessionId, snapshot: &Snapshot) -> Option<String> {
        if let Some(session) = self.sessions.iter_mut().find(|session| session.id == id) {
            session.conversation.reset(snapshot);
            session.state = SessionState::Live;
            return session.pending_post.clone();
        }
        None
    }

    pub(crate) fn acknowledge_pending_post(&mut self, id: SessionId) {
        if let Some(session) = self.sessions.iter_mut().find(|session| session.id == id) {
            session.pending_post = None;
        }
    }

    pub(crate) fn restore_pending_post(&mut self, id: SessionId) {
        let Some(session) = self.sessions.iter_mut().find(|session| session.id == id) else {
            return;
        };
        let Some(pending) = session.pending_post.take() else {
            return;
        };
        let draft = session.conversation.composer.lines().join("\n");
        session.conversation.clear_composer();
        session.conversation.composer.insert_str(pending);
        if !draft.trim().is_empty() {
            session.conversation.composer.insert_str("\n\n");
            session.conversation.composer.insert_str(draft);
        }
    }

    pub(crate) fn apply(&mut self, id: SessionId, step: &StepEvent) {
        let Some(index) = self.sessions.iter().position(|session| session.id == id) else {
            return;
        };
        let attention = self.sessions[index].conversation.apply(step);
        if index != self.current || self.focus == Focus::Sessions {
            self.sessions[index].background_unread |= attention;
        }
    }

    pub(crate) fn reset(&mut self, id: SessionId, snapshot: &Snapshot) {
        let Some(index) = self.sessions.iter().position(|session| session.id == id) else {
            return;
        };
        let attention = self.sessions[index].conversation.reset(snapshot);
        if index != self.current || self.focus == Focus::Sessions {
            self.sessions[index].background_unread |= attention;
        }
    }

    pub(crate) fn mark_offline(&mut self, id: SessionId, reason: impl Into<String>) {
        if let Some(session) = self.sessions.iter_mut().find(|session| session.id == id)
            && !matches!(&session.state, SessionState::Offline(_))
        {
            session.state = SessionState::Offline(reason.into());
        }
    }

    pub(crate) fn set_viewport(&mut self, viewport: Viewport) {
        self.viewport = viewport;
    }

    pub(crate) fn on_event(&mut self, event: Event) -> Action {
        if let Event::Key(key) = &event
            && key.kind == KeyEventKind::Press
        {
            let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
            match (key.code, ctrl) {
                (KeyCode::Char('c'), true) => return Action::Quit,
                (KeyCode::Char('n'), true) => return Action::NewSession,
                (KeyCode::Left, true) if self.focus == Focus::Composer => {
                    self.focus = Focus::Sessions;
                    return Action::None;
                }
                (KeyCode::Right, true) if self.focus == Focus::Sessions => {
                    self.focus_composer();
                    return Action::None;
                }
                _ => {}
            }
        }

        if self.focus == Focus::Sessions {
            if let Event::Key(key) = &event
                && key.kind == KeyEventKind::Press
                && key.modifiers == KeyModifiers::NONE
            {
                match key.code {
                    KeyCode::Up => {
                        self.select_previous();
                        return Action::None;
                    }
                    KeyCode::Down => {
                        self.select_next();
                        return Action::None;
                    }
                    KeyCode::Enter | KeyCode::Esc | KeyCode::Right => {
                        self.focus_composer();
                        return Action::None;
                    }
                    _ => {}
                }
            }

            let resumes_composing = matches!(&event, Event::Paste(_))
                || matches!(
                    &event,
                    Event::Key(key)
                        if key.kind != KeyEventKind::Release
                            && matches!(
                                key.code,
                                KeyCode::Char(_) | KeyCode::Backspace | KeyCode::Delete
                            )
                );
            if !resumes_composing {
                return Action::None;
            }
            self.focus_composer();
        }

        let session = &mut self.sessions[self.current];
        match session.conversation.on_event(event, self.viewport) {
            ConversationAction::None => Action::None,
            ConversationAction::Post(text) if session.state == SessionState::Live => Action::Post {
                id: session.id,
                text,
            },
            ConversationAction::Post(text)
                if session.state == SessionState::Opening && session.pending_post.is_none() =>
            {
                session.pending_post = Some(text);
                session.conversation.clear_composer();
                Action::None
            }
            ConversationAction::Stop { clear } if session.state == SessionState::Live => {
                Action::Stop {
                    id: session.id,
                    clear,
                }
            }
            ConversationAction::Post(_) | ConversationAction::Stop { .. } => Action::None,
            ConversationAction::Quit => Action::Quit,
        }
    }

    pub(crate) fn clear_composer(&mut self, id: SessionId) {
        if let Some(session) = self.sessions.iter_mut().find(|session| session.id == id) {
            session.conversation.clear_composer();
        }
    }

    pub(crate) fn current(&self) -> &SessionUi {
        &self.sessions[self.current]
    }

    fn select_previous(&mut self) {
        if self.sessions.len() > 1 {
            self.select((self.current + self.sessions.len() - 1) % self.sessions.len());
        }
    }

    fn select_next(&mut self) {
        if self.sessions.len() > 1 {
            self.select((self.current + 1) % self.sessions.len());
        }
    }

    fn select(&mut self, index: usize) {
        self.current = index;
    }

    fn focus_composer(&mut self) {
        self.focus = Focus::Composer;
        self.sessions[self.current].background_unread = false;
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Focus {
    Sessions,
    #[default]
    Composer,
}

pub(crate) struct SessionUi {
    pub(crate) id: SessionId,
    pub(crate) conversation: Conversation,
    pub(crate) background_unread: bool,
    pub(crate) state: SessionState,
    pub(crate) pending_post: Option<String>,
}

impl SessionUi {
    pub(crate) fn title(&self) -> &str {
        self.conversation
            .projection
            .title()
            .or_else(|| {
                self.pending_post
                    .as_deref()
                    .and_then(|text| text.lines().find(|line| !line.trim().is_empty()))
            })
            .or_else(|| {
                self.conversation
                    .composer
                    .lines()
                    .iter()
                    .find(|line| !line.trim().is_empty())
                    .map(String::as_str)
            })
            .unwrap_or("New conversation")
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SessionState {
    Opening,
    Live,
    Offline(String),
}

pub(crate) struct Conversation {
    pub(crate) projection: Projection,
    pub(crate) composer: TextArea<'static>,
    pub(crate) anchor: Option<ScrollAnchor>,
    pub(crate) unread: bool,
    cursor: u64,
}

impl Conversation {
    fn empty(show_progress: bool) -> Self {
        Self {
            projection: Projection::empty(show_progress),
            composer: composer(),
            anchor: None,
            unread: false,
            cursor: 0,
        }
    }

    fn new(snapshot: &Snapshot, show_progress: bool) -> Self {
        let mut conversation = Self::empty(show_progress);
        conversation.projection = Projection::from_snapshot(snapshot, show_progress);
        conversation.cursor = snapshot.record_cursor;
        conversation
    }

    fn apply(&mut self, step: &StepEvent) -> bool {
        let before = self.projection.timeline.len();
        self.projection.apply_all(&step.records);
        if let Some(entry) = step.records.last() {
            self.cursor = entry.cursor;
        }
        let added = &self.projection.timeline[before..];
        if self.anchor.is_some() && !added.is_empty() {
            self.unread = true;
        }
        added.iter().any(|item| item.attention) || step.records.iter().any(is_finished_notice)
    }

    fn reset(&mut self, snapshot: &Snapshot) -> bool {
        let latest = self.cursor;
        let show_progress = self.projection.show_progress;
        self.projection = Projection::from_snapshot(snapshot, show_progress);
        self.cursor = snapshot.record_cursor;
        let first = self
            .projection
            .timeline
            .partition_point(|item| item.cursor <= latest);
        let new_items = &self.projection.timeline[first..];
        let attention = new_items.iter().any(|item| item.attention)
            || snapshot
                .record
                .iter()
                .any(|entry| entry.cursor > latest && is_finished_notice(entry));

        if let Some(anchor) = self.anchor {
            self.anchor = self
                .projection
                .timeline
                .iter()
                .find(|item| item.cursor >= anchor.cursor)
                .map(|item| ScrollAnchor {
                    cursor: item.cursor,
                    line: anchor.line,
                });
            self.unread |= !new_items.is_empty();
        } else {
            self.unread = false;
        }
        attention
    }

    fn on_event(&mut self, event: Event, viewport: Viewport) -> ConversationAction {
        match event {
            Event::Key(key) => self.on_key(key, viewport),
            Event::Paste(text) => {
                self.composer
                    .insert_str(text.replace("\r\n", "\n").replace('\r', "\n"));
                ConversationAction::None
            }
            _ => ConversationAction::None,
        }
    }

    fn clear_composer(&mut self) {
        self.composer = composer();
    }

    fn on_key(&mut self, key: KeyEvent, viewport: Viewport) -> ConversationAction {
        if key.kind == KeyEventKind::Release {
            return ConversationAction::None;
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if key.kind == KeyEventKind::Press {
            match (key.code, ctrl) {
                (KeyCode::Char('j'), true) => {
                    self.composer.insert_newline();
                    return ConversationAction::None;
                }
                (KeyCode::Esc, _) => return ConversationAction::Stop { clear: false },
                (KeyCode::PageUp, _) => {
                    self.scroll_up(viewport);
                    return ConversationAction::None;
                }
                (KeyCode::PageDown, _) => {
                    self.scroll_down(viewport);
                    return ConversationAction::None;
                }
                (KeyCode::Home, true) => {
                    self.scroll_to_top();
                    return ConversationAction::None;
                }
                (KeyCode::End, true) => {
                    self.scroll_to_end();
                    return ConversationAction::None;
                }
                (KeyCode::Enter, false) => return self.submit(),
                _ => {}
            }
        } else if key.code == KeyCode::Enter {
            return ConversationAction::None;
        }

        self.composer.input(key);
        ConversationAction::None
    }

    fn submit(&self) -> ConversationAction {
        let text = self.composer.lines().join("\n");
        match text.trim() {
            "" => ConversationAction::None,
            "/stop" => ConversationAction::Stop { clear: true },
            "/exit" => ConversationAction::Quit,
            _ => ConversationAction::Post(text),
        }
    }

    fn scroll_up(&mut self, viewport: Viewport) {
        let Some(layout) = TimelineLayout::new(&self.projection.timeline, viewport) else {
            return;
        };
        let current = layout.top(self.anchor);
        if current == 0 {
            return;
        }
        self.anchor = Some(layout.anchor(current.saturating_sub(viewport.page())));
    }

    fn scroll_down(&mut self, viewport: Viewport) {
        let Some(anchor) = self.anchor else {
            return;
        };
        let Some(layout) = TimelineLayout::new(&self.projection.timeline, viewport) else {
            return;
        };
        let next = layout.top(Some(anchor)).saturating_add(viewport.page());
        if next >= layout.tail {
            self.scroll_to_end();
        } else {
            self.anchor = Some(layout.anchor(next));
        }
    }

    fn scroll_to_top(&mut self) {
        self.anchor = self.projection.timeline.first().map(|item| ScrollAnchor {
            cursor: item.cursor,
            line: 0,
        });
    }

    fn scroll_to_end(&mut self) {
        self.anchor = None;
        self.unread = false;
    }
}

fn is_finished_notice(entry: &RecordEntry) -> bool {
    matches!(entry.kind, RecordKind::Notice(Notice::Finished { .. }))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Viewport {
    pub(crate) width: u16,
    pub(crate) height: u16,
    pub(crate) spacing: usize,
}

impl Viewport {
    fn page(self) -> usize {
        usize::from(self.height).saturating_sub(1).max(1)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ScrollAnchor {
    pub(crate) cursor: u64,
    pub(crate) line: usize,
}

struct TimelineLayout<'a> {
    rows: Vec<(&'a TimelineItem, usize, usize)>,
    tail: usize,
}

impl<'a> TimelineLayout<'a> {
    fn new(items: &'a [TimelineItem], viewport: Viewport) -> Option<Self> {
        if items.is_empty() || viewport.width == 0 || viewport.height == 0 {
            return None;
        }
        let mut top = 0;
        let rows = items
            .iter()
            .map(|item| {
                let height = item.line_count(viewport.width);
                let row = (item, top, height);
                top += height + viewport.spacing;
                row
            })
            .collect::<Vec<_>>();
        top = top.saturating_sub(viewport.spacing);
        Some(Self {
            rows,
            tail: top.saturating_sub(usize::from(viewport.height)),
        })
    }

    fn top(&self, anchor: Option<ScrollAnchor>) -> usize {
        let Some(anchor) = anchor else {
            return self.tail;
        };
        self.rows
            .iter()
            .find(|(item, _, _)| item.cursor == anchor.cursor)
            .map_or(self.tail, |(_, top, height)| {
                top + anchor.line.min(height.saturating_sub(1))
            })
    }

    fn anchor(&self, target: usize) -> ScrollAnchor {
        for (item, top, height) in &self.rows {
            if target < top + height {
                return ScrollAnchor {
                    cursor: item.cursor,
                    line: target.saturating_sub(*top),
                };
            }
        }
        let (item, _, height) = self.rows.last().expect("a timeline row exists");
        ScrollAnchor {
            cursor: item.cursor,
            line: height.saturating_sub(1),
        }
    }
}

fn composer() -> TextArea<'static> {
    let mut composer = TextArea::default();
    composer.set_wrap_mode(WrapMode::WordOrGlyph);
    composer.set_placeholder_text("Ask BONE…");
    composer.set_placeholder_style(Style::default().dim());
    composer.set_cursor_line_style(Style::default());
    composer
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Action {
    None,
    Post { id: SessionId, text: String },
    Stop { id: SessionId, clear: bool },
    NewSession,
    Quit,
}

#[derive(Debug, PartialEq, Eq)]
enum ConversationAction {
    None,
    Post(String),
    Stop { clear: bool },
    Quit,
}

#[derive(Debug, PartialEq)]
pub(crate) struct Projection {
    pub(crate) timeline: Vec<TimelineItem>,
    pub(crate) active: BTreeMap<JobId, ActiveJob>,
    pub(crate) status: SessionStatus,
    show_progress: bool,
    tool_calls: BTreeMap<JobId, ToolCall>,
    unknown_tools: BTreeSet<JobId>,
}

impl Projection {
    fn empty(show_progress: bool) -> Self {
        Self {
            timeline: Vec::new(),
            active: BTreeMap::new(),
            status: SessionStatus::Ready,
            show_progress,
            tool_calls: BTreeMap::new(),
            unknown_tools: BTreeSet::new(),
        }
    }

    fn from_snapshot(snapshot: &Snapshot, show_progress: bool) -> Self {
        let mut projection = Self::empty(show_progress);
        projection.apply_all(&snapshot.record);
        if projection.active.is_empty()
            && projection.status == SessionStatus::Ready
            && snapshot.autonomous
        {
            projection.status = SessionStatus::Waiting;
        }
        projection
    }

    fn apply_all(&mut self, records: &[RecordEntry]) {
        for entry in records {
            self.apply(entry);
        }
    }

    fn apply(&mut self, entry: &RecordEntry) {
        match &entry.kind {
            RecordKind::UserMessage(message) => {
                self.status = SessionStatus::Working;
                self.timeline.push(TimelineItem::message(
                    entry.cursor,
                    Speaker::User,
                    message.text.clone(),
                    false,
                ));
            }
            RecordKind::CancellationRequested { job } => {
                if let Some(active) = self.active.get_mut(job) {
                    active.stopping = true;
                }
            }
            RecordKind::Notice(notice) => self.apply_notice(entry.cursor, notice),
            _ => {}
        }
    }

    fn apply_notice(&mut self, cursor: u64, notice: &Notice) {
        match notice {
            Notice::Reply { text, .. } => self.timeline.push(TimelineItem::message(
                cursor,
                Speaker::Bone,
                text.clone(),
                true,
            )),
            Notice::JobStarted { id, request } => {
                let kind = match request {
                    JobRequest::Work { .. } => ActiveKind::Work,
                    JobRequest::ReviewInput { .. } => ActiveKind::Review,
                    JobRequest::Tool(call) => {
                        let active = active_tool(call);
                        self.tool_calls.insert(*id, call.clone());
                        ActiveKind::Tool(active)
                    }
                };
                self.active.insert(*id, ActiveJob::new(kind));
                self.status = SessionStatus::Working;
            }
            Notice::JobProgress { id, progress } => {
                if let Some(active) = self.active.get_mut(id) {
                    active.progress = Some(progress.clone());
                }
            }
            Notice::JobFinished { id, outcome } => {
                self.active.remove(id);
                if self.active.is_empty() && self.status == SessionStatus::Working {
                    self.status = SessionStatus::Waiting;
                }
                let Some(call) = self.tool_calls.get(id).cloned() else {
                    return;
                };
                let resolved = self.unknown_tools.remove(id);
                if outcome.external_effect == ExternalEffect::Unknown {
                    self.unknown_tools.insert(*id);
                } else {
                    self.tool_calls.remove(id);
                }
                if !resolved
                    && outcome.external_effect == ExternalEffect::None
                    && matches!(
                        &outcome.result,
                        Err(error) if error.kind == JobErrorKind::Cancelled
                    )
                {
                    return;
                }
                let (text, tone, attention) = tool_result(&call, outcome, resolved);
                if self.show_progress || attention {
                    self.timeline.push(TimelineItem {
                        cursor,
                        kind: TimelineKind::Tool { text, tone },
                        attention,
                    });
                }
            }
            Notice::Error { message } => {
                self.timeline.push(TimelineItem {
                    cursor,
                    kind: TimelineKind::Error(message.clone()),
                    attention: true,
                });
            }
            Notice::Paused => {
                self.status = SessionStatus::Waiting;
                self.timeline.push(TimelineItem::status(
                    cursor,
                    "— waiting for you",
                    Tone::Warning,
                    true,
                ));
            }
            Notice::Stopped => {
                self.status = SessionStatus::Stopped;
                self.timeline.push(TimelineItem::status(
                    cursor,
                    "— work stopped",
                    Tone::Warning,
                    true,
                ));
            }
            Notice::Finished { .. } => {
                self.status = SessionStatus::Complete;
            }
        }
    }

    pub(crate) fn activity(&self) -> Option<String> {
        if !self.show_progress {
            return None;
        }
        let stopping = self.active.values().filter(|job| job.stopping).count();
        if stopping > 0 {
            return Some(format!(
                "stopping · {} {} resolving",
                self.active.len(),
                if self.active.len() == 1 {
                    "job"
                } else {
                    "jobs"
                }
            ));
        }

        if self.active.is_empty() {
            return None;
        }
        if self.active.len() == 1 {
            return self.active.values().next().map(ActiveJob::detail);
        }

        let mut parts = self
            .active
            .values()
            .take(2)
            .map(ActiveJob::summary)
            .collect::<Vec<_>>();
        let more = self.active.len().saturating_sub(parts.len());
        if more > 0 {
            parts.push(format!("+{more}"));
        }
        Some(format!(
            "{} active · {}",
            self.active.len(),
            parts.join(" · ")
        ))
    }

    pub(crate) fn has_unknown_effect(&self) -> bool {
        !self.unknown_tools.is_empty()
    }

    pub(crate) fn title(&self) -> Option<&str> {
        self.timeline.iter().find_map(|item| match &item.kind {
            TimelineKind::Message {
                speaker: Speaker::User,
                text,
            } => text.lines().find(|line| !line.trim().is_empty()),
            _ => None,
        })
    }
}

fn tool_result(
    call: &ToolCall,
    outcome: &bone_agent::JobOutcome,
    resolved: bool,
) -> (String, Tone, bool) {
    if outcome.external_effect == ExternalEffect::Unknown {
        let detail = match &outcome.result {
            Err(error) => format!(": {}", error.message),
            Ok(_) => String::new(),
        };
        return (
            format!("! {} · outcome unknown{detail}", tool_subject(call)),
            Tone::Warning,
            true,
        );
    }

    let resolution = resolved.then_some("outcome resolved");
    match &outcome.result {
        Ok(_) => (
            match resolution {
                Some(detail) => format!("✓ {} · {detail}", completed_tool(call, outcome)),
                None => format!("✓ {}", completed_tool(call, outcome)),
            },
            Tone::Success,
            resolved,
        ),
        Err(error) => {
            let detail = match error.kind {
                JobErrorKind::Cancelled => "cancelled",
                JobErrorKind::TimedOut => "timed out",
                JobErrorKind::Failed | JobErrorKind::Panicked => error.message.as_str(),
            };
            let detail = match resolution {
                Some(resolution) => format!("{resolution}: {detail}"),
                None => detail.to_owned(),
            };
            let symbol = if error.kind == JobErrorKind::Cancelled {
                "—"
            } else {
                "×"
            };
            (
                format!("{symbol} {} · {detail}", tool_subject(call)),
                if error.kind == JobErrorKind::Cancelled {
                    Tone::Warning
                } else {
                    Tone::Error
                },
                resolved || error.kind != JobErrorKind::Cancelled,
            )
        }
    }
}

fn tool_subject(call: &ToolCall) -> String {
    match call.name.as_str() {
        "read" => display_argument(call, "path").unwrap_or_else(|| "file".to_owned()),
        "grep" => match (
            display_argument(call, "pattern"),
            display_argument(call, "path"),
        ) {
            (Some(pattern), Some(path)) => format!("search {pattern:?} in {path}"),
            (Some(pattern), None) => format!("search {pattern:?}"),
            _ => "grep".to_owned(),
        },
        "glob" => match (
            display_argument(call, "pattern"),
            display_argument(call, "path"),
        ) {
            (Some(pattern), Some(path)) => format!("find {pattern} in {path}"),
            (Some(pattern), None) => format!("find {pattern}"),
            _ => "find files".to_owned(),
        },
        name => humanized_tool_name(name),
    }
}

fn active_tool(call: &ToolCall) -> String {
    let subject = tool_subject(call);
    match call.name.as_str() {
        "read" => format!("Reading {subject}"),
        "grep" => format!(
            "Searching {}",
            subject.strip_prefix("search ").unwrap_or(&subject)
        ),
        "glob" => format!(
            "Finding {}",
            subject.strip_prefix("find ").unwrap_or(&subject)
        ),
        _ => subject,
    }
}

fn completed_tool(call: &ToolCall, outcome: &bone_agent::JobOutcome) -> String {
    let artifact = match &outcome.result {
        Ok(JobOutput::Artifact(value)) => Some(value),
        _ => None,
    };
    match call.name.as_str() {
        "read" => {
            let path = display_argument(call, "path").unwrap_or_else(|| "file".to_owned());
            match artifact.and_then(|value| {
                Some((
                    value.get("start_line")?.as_u64()?,
                    value.get("end_line")?.as_u64()?,
                ))
            }) {
                Some((start, end)) => format!("Read {path} · lines {start}–{end}"),
                None => format!("Read {path}"),
            }
        }
        "grep" => {
            let subject = tool_subject(call);
            match artifact
                .and_then(|value| value.get("match_count"))
                .and_then(|value| value.as_u64())
            {
                Some(count) => format!(
                    "Searched {} · {count} matches",
                    subject.trim_start_matches("search ")
                ),
                None => format!("Searched {}", subject.trim_start_matches("search ")),
            }
        }
        "glob" => {
            let subject = tool_subject(call);
            match artifact
                .and_then(|value| value.get("paths"))
                .and_then(|value| value.as_array())
            {
                Some(paths) => format!(
                    "Found {} · {} paths",
                    subject.trim_start_matches("find "),
                    paths.len()
                ),
                None => format!("Found {}", subject.trim_start_matches("find ")),
            }
        }
        _ => tool_subject(call),
    }
}

fn display_argument(call: &ToolCall, key: &str) -> Option<String> {
    let value = call.arguments.get(key)?.as_str()?;
    let value = normalize_and_shorten(value);
    (!value.is_empty()).then_some(value)
}

fn humanized_tool_name(name: &str) -> String {
    let name = name.replace(['_', '-'], " ");
    let name = normalize_and_shorten(&name);
    if name.is_empty() {
        "tool".to_owned()
    } else {
        name
    }
}

fn normalize_and_shorten(text: &str) -> String {
    const LIMIT: usize = 48;
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = normalized.chars();
    let short = chars.by_ref().take(LIMIT).collect::<String>();
    if chars.next().is_some() {
        format!("{short}…")
    } else {
        short
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct TimelineItem {
    pub(crate) cursor: u64,
    pub(crate) kind: TimelineKind,
    pub(crate) attention: bool,
}

impl TimelineItem {
    fn message(cursor: u64, speaker: Speaker, text: String, attention: bool) -> Self {
        Self {
            cursor,
            kind: TimelineKind::Message { speaker, text },
            attention,
        }
    }

    fn status(cursor: u64, text: impl Into<String>, tone: Tone, attention: bool) -> Self {
        Self {
            cursor,
            kind: TimelineKind::Status {
                text: text.into(),
                tone,
            },
            attention,
        }
    }

    pub(crate) fn line_count(&self, width: u16) -> usize {
        Paragraph::new(self.text())
            .wrap(Wrap { trim: false })
            .line_count(width.saturating_sub(2).max(1))
            .max(1)
    }

    fn text(&self) -> &str {
        match &self.kind {
            TimelineKind::Message { text, .. }
            | TimelineKind::Tool { text, .. }
            | TimelineKind::Error(text)
            | TimelineKind::Status { text, .. } => text,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum TimelineKind {
    Message { speaker: Speaker, text: String },
    Tool { text: String, tone: Tone },
    Error(String),
    Status { text: String, tone: Tone },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Speaker {
    User,
    Bone,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tone {
    Success,
    Warning,
    Error,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ActiveJob {
    kind: ActiveKind,
    progress: Option<JobProgress>,
    stopping: bool,
}

impl ActiveJob {
    fn new(kind: ActiveKind) -> Self {
        Self {
            kind,
            progress: None,
            stopping: false,
        }
    }

    fn summary(&self) -> String {
        let label = match &self.kind {
            ActiveKind::Work => "Thinking".to_owned(),
            ActiveKind::Review => "Reading your update".to_owned(),
            ActiveKind::Tool(label) => label.clone(),
        };
        match self.progress.as_ref().and_then(|progress| progress.percent) {
            Some(percent) => format!("{label} {percent}%"),
            None => label,
        }
    }

    fn detail(&self) -> String {
        let summary = self.summary();
        let Some(progress) = &self.progress else {
            return summary;
        };
        let message = progress
            .message
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if message.is_empty() {
            summary
        } else {
            format!("{summary} · {message}")
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ActiveKind {
    Work,
    Review,
    Tool(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionStatus {
    Ready,
    Working,
    Waiting,
    Stopped,
    Complete,
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use bone_agent::{
        EffectSummary, Event as AgentEvent, JobOutcome, Message, MessageId, Notice, RecordEntry,
        RecordKind, Snapshot, StepEvent, ToolCall,
    };
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    use super::{
        Action, App, Focus, Projection, ScrollAnchor, SessionId, SessionState, SessionStatus,
        Speaker, TimelineKind, Viewport, active_tool,
    };

    #[test]
    fn composer_distinguishes_send_newline_repeat_and_stop() {
        let mut app = App::new("workspace".into());
        app.add_session(SessionId(1), &snapshot(vec![]), true);
        app.on_event(Event::Paste("你好\r\n🙂e\u{301}\rline".into()));
        assert_eq!(
            app.current().conversation.composer.lines(),
            ["你好", "🙂e\u{301}", "line"]
        );

        assert_eq!(
            app.on_event(Event::Key(key(
                KeyCode::Enter,
                KeyModifiers::NONE,
                KeyEventKind::Repeat,
            ))),
            Action::None
        );
        app.on_event(Event::Key(key(
            KeyCode::Char('j'),
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        )));
        assert_eq!(app.current().conversation.composer.lines().len(), 4);
        assert!(matches!(
            app.on_event(Event::Key(key(
                KeyCode::Enter,
                KeyModifiers::NONE,
                KeyEventKind::Press,
            ))),
            Action::Post { id: SessionId(1), text }
                if text == "你好\n🙂e\u{301}\nline\n"
        ));

        let draft = app.current().conversation.composer.lines().to_vec();
        assert_eq!(
            app.on_event(Event::Key(key(
                KeyCode::Esc,
                KeyModifiers::NONE,
                KeyEventKind::Press,
            ))),
            Action::Stop {
                id: SessionId(1),
                clear: false,
            }
        );
        assert_eq!(app.current().conversation.composer.lines(), draft);
    }

    #[test]
    fn new_conversation_always_means_new_conversation() {
        let mut app = App::new("workspace".into());
        app.add_session(SessionId(1), &snapshot(vec![]), true);

        assert_eq!(
            app.on_event(Event::Key(key(
                KeyCode::Char('n'),
                KeyModifiers::CONTROL,
                KeyEventKind::Press,
            ))),
            Action::NewSession
        );
        assert_eq!(app.current().id, SessionId(1));
    }

    #[test]
    fn incremental_projection_matches_snapshot_rebuild() {
        let records = conversation_records();
        let mut incremental = Projection::from_snapshot(&snapshot(vec![]), true);
        incremental.apply_all(&records[..3]);
        incremental.apply_all(&records[3..]);
        let rebuilt = Projection::from_snapshot(&snapshot(records), true);
        assert_eq!(incremental, rebuilt);
    }

    #[test]
    fn reset_keeps_the_draft_anchor_and_counts_missed_items() {
        let initial = vec![user(1, "one"), reply(2, "two")];
        let mut app = App::new("workspace".into());
        app.add_session(SessionId(1), &snapshot(initial.clone()), true);
        app.on_event(Event::Paste("unfinished 草稿".into()));
        app.sessions[0].conversation.anchor = Some(ScrollAnchor { cursor: 1, line: 0 });
        app.sessions[0].conversation.unread = true;

        let mut recovered = initial;
        recovered.push(user(3, "three"));
        recovered.push(reply(4, "four"));
        app.reset(SessionId(1), &snapshot(recovered));

        let conversation = &app.current().conversation;
        assert_eq!(conversation.composer.lines(), ["unfinished 草稿"]);
        assert_eq!(
            conversation.anchor,
            Some(ScrollAnchor { cursor: 1, line: 0 })
        );
        assert!(conversation.unread);
    }

    #[test]
    fn background_updates_and_session_switching_keep_each_draft_independent() {
        let mut app = App::new("workspace".into());
        app.add_session(SessionId(1), &snapshot(vec![user(1, "first")]), true);
        app.on_event(Event::Paste("draft one".into()));
        app.add_session(SessionId(2), &snapshot(vec![]), true);
        app.on_event(Event::Paste("draft two".into()));

        app.apply(
            SessionId(1),
            &step(vec![reply(2, "finished in background")]),
        );
        assert!(app.sessions[0].background_unread);
        assert_eq!(app.sessions[1].conversation.composer.lines(), ["draft two"]);

        app.on_event(Event::Key(key(
            KeyCode::Left,
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        )));
        app.on_event(Event::Key(key(
            KeyCode::Up,
            KeyModifiers::NONE,
            KeyEventKind::Press,
        )));
        assert_eq!(app.current().id, SessionId(1));
        assert!(app.current().background_unread);
        app.on_event(Event::Key(key(
            KeyCode::Right,
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        )));
        assert_eq!(app.current().id, SessionId(1));
        assert!(!app.current().background_unread);
        assert_eq!(app.current().conversation.composer.lines(), ["draft one"]);
    }

    #[test]
    fn session_focus_keeps_new_activity_unread_until_returning_to_composer() {
        let mut app = App::new("workspace".into());
        app.add_session(SessionId(1), &snapshot(vec![user(1, "first")]), true);

        app.on_event(Event::Key(key(
            KeyCode::Left,
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        )));
        app.apply(SessionId(1), &step(vec![reply(2, "arrived in the list")]));
        assert!(app.current().background_unread);

        app.on_event(Event::Key(key(
            KeyCode::Right,
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        )));
        assert!(!app.current().background_unread);
    }

    #[test]
    fn background_finish_is_unread_without_becoming_a_timeline_row() {
        let mut app = App::new("workspace".into());
        app.add_session(SessionId(1), &snapshot(vec![user(1, "first")]), true);
        app.add_session(SessionId(2), &snapshot(vec![user(2, "second")]), true);
        let timeline_len = app.sessions[0].conversation.projection.timeline.len();

        app.apply(
            SessionId(1),
            &step(vec![RecordEntry {
                cursor: 2,
                kind: RecordKind::Notice(Notice::Finished { cleanup: vec![] }),
            }]),
        );

        assert!(app.sessions[0].background_unread);
        assert_eq!(
            app.sessions[0].conversation.projection.status,
            SessionStatus::Complete
        );
        assert_eq!(
            app.sessions[0].conversation.projection.timeline.len(),
            timeline_len
        );
    }

    #[test]
    fn quiet_mode_hides_success_but_keeps_failed_and_unknown_tools() {
        let records = vec![
            tool_started(1, 1, "read"),
            tool_finished(2, 1, JobOutcome::artifact("ok")),
            tool_started(3, 2, "grep"),
            tool_finished(4, 2, JobOutcome::failed("pattern rejected")),
            tool_started(5, 3, "write"),
            tool_finished(6, 3, JobOutcome::unknown("connection lost")),
            tool_finished(7, 3, JobOutcome::artifact("confirmed")),
        ];
        let projection = Projection::from_snapshot(&snapshot(records), false);
        assert_eq!(projection.timeline.len(), 3);
        assert!(matches!(
            &projection.timeline[0].kind,
            TimelineKind::Tool { text, .. } if text == "× grep · pattern rejected"
        ));
        assert!(matches!(
            &projection.timeline[1].kind,
            TimelineKind::Tool { text, .. }
                if text == "! write · outcome unknown: connection lost"
        ));
        assert!(matches!(
            &projection.timeline[2].kind,
            TimelineKind::Tool { text, .. } if text == "✓ write · outcome resolved"
        ));
        assert!(!projection.has_unknown_effect());
    }

    #[test]
    fn ordinary_tool_cancellation_does_not_look_like_a_failed_result() {
        let cancelled = JobOutcome {
            result: Err(bone_agent::JobError {
                kind: bone_agent::JobErrorKind::Cancelled,
                message: "cancelled during cleanup".into(),
            }),
            external_effect: bone_agent::ExternalEffect::None,
        };
        let projection = Projection::from_snapshot(
            &snapshot(vec![
                tool_started(1, 1, "read"),
                tool_finished(2, 1, cancelled),
            ]),
            true,
        );

        assert!(projection.timeline.is_empty());
        assert!(!projection.has_unknown_effect());
    }

    #[test]
    fn cancelled_write_with_an_unknown_outcome_stays_visible() {
        let cancelled = JobOutcome {
            result: Err(bone_agent::JobError {
                kind: bone_agent::JobErrorKind::Cancelled,
                message: "cancel refused after commit started".into(),
            }),
            external_effect: bone_agent::ExternalEffect::Unknown,
        };
        let projection = Projection::from_snapshot(
            &snapshot(vec![
                tool_started(1, 1, "write"),
                tool_finished(2, 1, cancelled),
            ]),
            false,
        );

        assert!(projection.has_unknown_effect());
        assert!(matches!(
            &projection.timeline[0].kind,
            TimelineKind::Tool { text, .. }
                if text == "! write · outcome unknown: cancel refused after commit started"
        ));
    }

    #[test]
    fn opening_session_owns_its_draft_and_attach_does_not_steal_focus() {
        let mut app = App::new("workspace".into());
        app.add_session(SessionId(1), &snapshot(vec![user(1, "first")]), true);

        assert_eq!(
            app.on_event(Event::Key(key(
                KeyCode::Char('n'),
                KeyModifiers::CONTROL,
                KeyEventKind::Press,
            ))),
            Action::NewSession
        );
        app.begin_session(SessionId(2), true);
        app.on_event(Event::Paste("draft for two".into()));
        assert_eq!(
            app.on_event(Event::Key(key(
                KeyCode::Enter,
                KeyModifiers::NONE,
                KeyEventKind::Press,
            ))),
            Action::None
        );

        app.on_event(Event::Key(key(
            KeyCode::Left,
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        )));
        app.on_event(Event::Key(key(
            KeyCode::Up,
            KeyModifiers::NONE,
            KeyEventKind::Press,
        )));
        app.on_event(Event::Key(key(
            KeyCode::Right,
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        )));
        let pending = app.attach(SessionId(2), &snapshot(vec![]));

        assert_eq!(app.current().id, SessionId(1));
        assert_eq!(app.sessions[1].state, SessionState::Live);
        assert_eq!(pending.as_deref(), Some("draft for two"));
        assert_eq!(app.sessions[1].conversation.composer.lines(), [""]);

        app.mark_offline(SessionId(2), "connection closed");
        assert_eq!(app.sessions[0].state, SessionState::Live);
        assert_eq!(
            app.sessions[1].state,
            SessionState::Offline("connection closed".into())
        );
    }

    #[test]
    fn pending_post_is_kept_until_acknowledged() {
        let mut app = App::new("workspace".into());
        app.begin_session(SessionId(1), true);
        app.on_event(Event::Paste("send after opening".into()));
        app.on_event(Event::Key(key(
            KeyCode::Enter,
            KeyModifiers::NONE,
            KeyEventKind::Press,
        )));

        assert_eq!(
            app.attach(SessionId(1), &snapshot(vec![])).as_deref(),
            Some("send after opening")
        );
        assert_eq!(
            app.sessions[0].pending_post.as_deref(),
            Some("send after opening")
        );

        app.acknowledge_pending_post(SessionId(1));
        assert_eq!(app.sessions[0].pending_post, None);
    }

    #[test]
    fn failed_pending_post_is_restored_before_the_new_draft() {
        let mut app = App::new("workspace".into());
        app.begin_session(SessionId(1), true);
        app.on_event(Event::Paste("failed message".into()));
        app.on_event(Event::Key(key(
            KeyCode::Enter,
            KeyModifiers::NONE,
            KeyEventKind::Press,
        )));
        app.on_event(Event::Paste("draft written while opening".into()));
        assert_eq!(
            app.attach(SessionId(1), &snapshot(vec![])).as_deref(),
            Some("failed message")
        );

        app.restore_pending_post(SessionId(1));

        assert_eq!(app.sessions[0].pending_post, None);
        assert_eq!(
            app.sessions[0].conversation.composer.lines(),
            ["failed message", "", "draft written while opening"]
        );
    }

    #[test]
    fn session_focus_selects_and_returns_typed_input_to_the_composer() {
        let mut app = App::new("workspace".into());
        app.add_session(SessionId(1), &snapshot(vec![user(1, "first")]), true);
        app.add_session(SessionId(2), &snapshot(vec![user(2, "second")]), true);
        app.sessions[0].background_unread = true;
        app.set_viewport(Viewport {
            width: 80,
            height: 20,
            spacing: 1,
        });

        app.on_event(Event::Key(key(
            KeyCode::Left,
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        )));
        assert_eq!(app.focus, Focus::Sessions);

        app.on_event(Event::Key(key(
            KeyCode::Up,
            KeyModifiers::NONE,
            KeyEventKind::Press,
        )));
        assert_eq!(app.current().id, SessionId(1));
        assert!(app.current().background_unread);

        app.on_event(Event::Key(key(
            KeyCode::Char('a'),
            KeyModifiers::NONE,
            KeyEventKind::Press,
        )));
        assert_eq!(app.focus, Focus::Composer);
        assert!(!app.current().background_unread);
        assert_eq!(app.current().conversation.composer.lines(), ["a"]);

        app.on_event(Event::Key(key(
            KeyCode::Left,
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        )));
        app.on_event(Event::Paste("bc".into()));
        assert_eq!(app.focus, Focus::Composer);
        assert_eq!(app.current().conversation.composer.lines(), ["abc"]);

        app.on_event(Event::Key(key(
            KeyCode::Left,
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        )));
        app.on_event(Event::Key(key(
            KeyCode::Backspace,
            KeyModifiers::NONE,
            KeyEventKind::Press,
        )));
        assert_eq!(app.focus, Focus::Composer);
        assert_eq!(app.current().conversation.composer.lines(), ["ab"]);

        app.on_event(Event::Key(key(
            KeyCode::Left,
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        )));
        app.on_event(Event::Key(key(
            KeyCode::Right,
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        )));
        assert_eq!(app.focus, Focus::Composer);
        app.on_event(Event::Paste(" kept".into()));
        assert_eq!(app.current().conversation.composer.lines(), ["ab kept"]);

        app.on_event(Event::Key(key(
            KeyCode::Left,
            KeyModifiers::NONE,
            KeyEventKind::Press,
        )));
        app.on_event(Event::Key(key(
            KeyCode::Left,
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        )));
        app.on_event(Event::Key(key(
            KeyCode::Delete,
            KeyModifiers::NONE,
            KeyEventKind::Press,
        )));
        assert_eq!(app.focus, Focus::Composer);
        assert_eq!(app.current().conversation.composer.lines(), ["ab kep"]);
    }

    #[test]
    fn narrow_view_still_enters_the_full_screen_session_list() {
        let mut app = App::new("workspace".into());
        app.add_session(SessionId(1), &snapshot(vec![user(1, "first")]), true);
        app.add_session(SessionId(2), &snapshot(vec![user(2, "second")]), true);
        app.set_viewport(Viewport {
            width: 40,
            height: 12,
            spacing: 1,
        });

        for (code, modifiers) in [
            (KeyCode::Up, KeyModifiers::CONTROL),
            (KeyCode::Up, KeyModifiers::ALT),
            (KeyCode::Char('1'), KeyModifiers::ALT),
        ] {
            app.on_event(Event::Key(key(code, modifiers, KeyEventKind::Press)));
            assert_eq!(app.current().id, SessionId(2));
        }

        app.on_event(Event::Key(key(
            KeyCode::Left,
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        )));
        assert_eq!(app.focus, Focus::Sessions);
        for modifiers in [KeyModifiers::CONTROL, KeyModifiers::ALT] {
            app.on_event(Event::Key(key(KeyCode::Up, modifiers, KeyEventKind::Press)));
            assert_eq!(app.current().id, SessionId(2));
        }
        app.on_event(Event::Key(key(
            KeyCode::Up,
            KeyModifiers::NONE,
            KeyEventKind::Press,
        )));
        assert_eq!(app.current().id, SessionId(1));
        app.on_event(Event::Key(key(
            KeyCode::Right,
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        )));
        assert_eq!(app.focus, Focus::Composer);
    }

    #[test]
    fn control_right_in_the_composer_is_forwarded_to_the_textarea() {
        let mut app = App::new("workspace".into());
        app.add_session(SessionId(1), &snapshot(vec![]), true);
        app.on_event(Event::Paste("one two".into()));
        app.on_event(Event::Key(key(
            KeyCode::Home,
            KeyModifiers::NONE,
            KeyEventKind::Press,
        )));
        assert_eq!(app.current().conversation.composer.cursor(), (0, 0));

        app.on_event(Event::Key(key(
            KeyCode::Right,
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        )));

        assert_eq!(app.focus, Focus::Composer);
        assert!(app.current().conversation.composer.cursor().1 > 0);
    }

    #[test]
    fn a_long_single_reply_scrolls_by_visual_rows() {
        let mut app = App::new("workspace".into());
        app.add_session(
            SessionId(1),
            &snapshot(vec![reply(1, &"很长的中文回复".repeat(60))]),
            true,
        );
        app.set_viewport(Viewport {
            width: 12,
            height: 5,
            spacing: 0,
        });

        for _ in 0..3 {
            app.on_event(Event::Key(key(
                KeyCode::PageUp,
                KeyModifiers::NONE,
                KeyEventKind::Press,
            )));
        }

        let anchor = app.current().conversation.anchor.unwrap();
        assert_eq!(anchor.cursor, 1);
        assert!(
            anchor.line > 0,
            "the viewport should reach inside the reply"
        );
        let middle = anchor.line;
        app.on_event(Event::Key(key(
            KeyCode::PageDown,
            KeyModifiers::NONE,
            KeyEventKind::Press,
        )));
        assert!(app.current().conversation.anchor.unwrap().line > middle);
    }

    #[test]
    fn page_up_on_short_history_keeps_following_the_live_tail() {
        let mut app = App::new("workspace".into());
        app.add_session(SessionId(1), &snapshot(vec![reply(1, "short")]), true);
        app.set_viewport(Viewport {
            width: 40,
            height: 10,
            spacing: 1,
        });

        app.on_event(Event::Key(key(
            KeyCode::PageUp,
            KeyModifiers::NONE,
            KeyEventKind::Press,
        )));
        assert_eq!(app.current().conversation.anchor, None);

        app.apply(SessionId(1), &step(vec![reply(2, "new reply")]));
        assert_eq!(app.current().conversation.anchor, None);
        assert!(!app.current().conversation.unread);
    }

    #[test]
    fn model_jobs_stay_in_activity_instead_of_the_timeline() {
        let records = vec![RecordEntry {
            cursor: 1,
            kind: RecordKind::Notice(Notice::JobStarted {
                id: bone_agent::JobId(1),
                request: bone_agent::JobRequest::Work { messages: vec![] },
            }),
        }];
        let projection = Projection::from_snapshot(&snapshot(records), true);
        assert!(projection.timeline.is_empty());
        assert_eq!(projection.activity().as_deref(), Some("Thinking"));
    }

    #[test]
    fn finishing_pure_reasoning_settles_into_waiting() {
        let mut state = snapshot(vec![
            RecordEntry {
                cursor: 1,
                kind: RecordKind::Notice(Notice::JobStarted {
                    id: bone_agent::JobId(1),
                    request: bone_agent::JobRequest::Work { messages: vec![] },
                }),
            },
            RecordEntry {
                cursor: 2,
                kind: RecordKind::Notice(Notice::JobFinished {
                    id: bone_agent::JobId(1),
                    outcome: JobOutcome::work(Default::default()),
                }),
            },
        ]);
        state.autonomous = true;

        let projection = Projection::from_snapshot(&state, true);
        assert_eq!(projection.status, SessionStatus::Waiting);
        assert!(projection.active.is_empty());
        assert_eq!(projection.activity(), None);
    }

    #[test]
    fn activity_uses_the_latest_progress_message_without_growing_history() {
        let records = vec![
            tool_started(1, 1, "read"),
            RecordEntry {
                cursor: 2,
                kind: RecordKind::Notice(Notice::JobProgress {
                    id: bone_agent::JobId(1),
                    progress: bone_agent::JobProgress {
                        message: "opening files".into(),
                        percent: None,
                    },
                }),
            },
            RecordEntry {
                cursor: 3,
                kind: RecordKind::Notice(Notice::JobProgress {
                    id: bone_agent::JobId(1),
                    progress: bone_agent::JobProgress {
                        message: "reading\nworkspace".into(),
                        percent: Some(42),
                    },
                }),
            },
        ];

        let projection = Projection::from_snapshot(&snapshot(records.clone()), true);
        assert_eq!(
            projection.activity().as_deref(),
            Some("Reading file 42% · reading workspace")
        );
        assert!(projection.timeline.is_empty());
        assert_eq!(
            Projection::from_snapshot(&snapshot(records), false).activity(),
            None
        );
    }

    #[test]
    fn tool_labels_normalize_and_bound_displayed_arguments() {
        let grep = ToolCall::new(
            "grep",
            serde_json::json!({
                "pattern": "  agent\n   loop  ",
                "path": "  crates/ bone-agent  "
            }),
        );
        assert_eq!(
            active_tool(&grep),
            "Searching \"agent loop\" in crates/ bone-agent"
        );

        let read = ToolCall::new(
            "read",
            serde_json::json!({"path": format!("  src/{}  ", "x".repeat(80))}),
        );
        let label = active_tool(&read);
        assert!(!label.contains("  "));
        assert!(label.ends_with('…'));
        assert_eq!(label.chars().count(), "Reading ".chars().count() + 49);

        let extension = ToolCall::new("future_tool-name", serde_json::json!({}));
        assert_eq!(active_tool(&extension), "future tool name");
    }

    fn key(code: KeyCode, modifiers: KeyModifiers, kind: KeyEventKind) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind,
            state: KeyEventState::NONE,
        }
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
            pending_messages: vec![],
            jobs: vec![],
            record,
            tools: vec![],
        }
    }

    fn conversation_records() -> Vec<RecordEntry> {
        vec![
            user(1, "Compare A and B"),
            RecordEntry {
                cursor: 2,
                kind: RecordKind::Notice(Notice::JobStarted {
                    id: bone_agent::JobId(1),
                    request: bone_agent::JobRequest::Work {
                        messages: vec![MessageId(1)],
                    },
                }),
            },
            reply(3, "I will compare them."),
            RecordEntry {
                cursor: 4,
                kind: RecordKind::Notice(Notice::Finished { cleanup: vec![] }),
            },
        ]
    }

    fn user(cursor: u64, text: &str) -> RecordEntry {
        RecordEntry {
            cursor,
            kind: RecordKind::UserMessage(Message {
                id: MessageId(cursor),
                text: text.into(),
            }),
        }
    }

    fn reply(cursor: u64, text: &str) -> RecordEntry {
        RecordEntry {
            cursor,
            kind: RecordKind::Notice(Notice::Reply {
                text: text.into(),
                reply_to: vec![],
                as_of: cursor,
            }),
        }
    }

    fn tool_started(cursor: u64, id: u64, name: &str) -> RecordEntry {
        RecordEntry {
            cursor,
            kind: RecordKind::Notice(Notice::JobStarted {
                id: bone_agent::JobId(id),
                request: bone_agent::JobRequest::Tool(ToolCall::new(name, serde_json::json!({}))),
            }),
        }
    }

    fn tool_finished(cursor: u64, id: u64, outcome: JobOutcome) -> RecordEntry {
        RecordEntry {
            cursor,
            kind: RecordKind::Notice(Notice::JobFinished {
                id: bone_agent::JobId(id),
                outcome,
            }),
        }
    }

    fn step(records: Vec<RecordEntry>) -> StepEvent {
        StepEvent {
            sequence: 1,
            elapsed: Duration::ZERO,
            event: AgentEvent::Stop,
            records,
            effects: Vec::<EffectSummary>::new(),
        }
    }

    #[test]
    fn message_kind_carries_the_expected_speaker() {
        let projection = Projection::from_snapshot(&snapshot(vec![user(1, "hello")]), true);
        assert!(matches!(
            projection.timeline[0].kind,
            TimelineKind::Message {
                speaker: Speaker::User,
                ..
            }
        ));
    }
}
