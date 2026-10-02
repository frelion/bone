use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Focus {
    Input,
    Conversation,
    Activity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Tone {
    Normal,
    Info,
    Success,
    Error,
}

#[derive(Debug, Clone)]
pub(super) struct Activity {
    pub event_id: String,
    pub title: String,
    pub detail: String,
    pub tone: Tone,
}

#[derive(Debug, Clone)]
pub(super) struct Message {
    pub role: String,
    pub text: String,
    pub event_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PickerKind {
    Command,
    Session,
    File,
    Model,
}
#[derive(Debug, Clone)]
pub(super) struct PickerItem {
    pub label: String,
    pub detail: String,
    pub value: String,
}
#[derive(Debug, Clone)]
pub(super) struct Picker {
    pub kind: PickerKind,
    pub query: String,
    pub selected: usize,
    pub items: Vec<PickerItem>,
}

#[derive(Debug)]
pub(super) struct View {
    pub draft: String,
    pub usage: String,
    pub session_label: String,
    pub busy: bool,
    pub live_status: String,
    pub picker: Option<Picker>,
    history: Vec<String>,
    history_index: Option<usize>,
    history_draft: String,
    undo: Vec<(String, usize)>,
    redo: Vec<(String, usize)>,
    search: Option<String>,
    search_selected: usize,
    search_total: usize,
    started: std::time::Instant,
    transcript_cache: Vec<Line<'static>>,
    cache_width: usize,
    cache_dirty: bool,
    pub messages: Vec<Message>,
    pub activities: Vec<Activity>,
    pub notice: String,
    pub focus: Focus,
    pub show_activity: bool,
    pub show_help: bool,
    pub detail: Option<(String, String)>,
    cursor: usize,
    conversation_scroll: usize,
    conversation_max: usize,
    follow_conversation: bool,
    selected: usize,
    activity_top: usize,
    activity_height: usize,
    detail_scroll: usize,
    detail_max: usize,
    terminal_width: u16,
    narrow_activity: bool,
}

const BACKGROUND: Color = Color::Reset;
const TEXT: Color = Color::Reset;
const MUTED: Color = Color::DarkGray;
const ACCENT: Color = Color::Cyan;

impl View {
    pub fn new() -> Self {
        Self {
            draft: String::new(),
            usage: String::new(),
            session_label: String::new(),
            busy: false,
            live_status: String::new(),
            picker: None,
            history: Vec::new(),
            history_index: None,
            history_draft: String::new(),
            undo: Vec::new(),
            redo: Vec::new(),
            search: None,
            search_selected: 0,
            search_total: 0,
            started: std::time::Instant::now(),
            transcript_cache: Vec::new(),
            cache_width: 80,
            cache_dirty: true,
            messages: Vec::new(),
            activities: Vec::new(),
            notice: String::new(),
            focus: Focus::Input,
            show_activity: false,
            show_help: false,
            detail: None,
            cursor: 0,
            conversation_scroll: 0,
            conversation_max: 0,
            follow_conversation: true,
            selected: 0,
            activity_top: 0,
            activity_height: 1,
            detail_scroll: 0,
            detail_max: 0,
            terminal_width: 120,
            narrow_activity: false,
        }
    }

    /// Drop terminal controls and ANSI CSI/OSC sequences, preserving ordinary text.
    pub fn sanitize(text: &str) -> String {
        let mut result = String::new();
        let mut chars = text.chars().peekable();
        while let Some(ch) = chars.next() {
            match ch {
                '\u{1b}' => match chars.peek().copied() {
                    Some('[') => {
                        chars.next();
                        for c in chars.by_ref() {
                            if ('@'..='~').contains(&c) {
                                break;
                            }
                        }
                    }
                    Some(']') => {
                        chars.next();
                        while let Some(c) = chars.next() {
                            if c == '\u{7}' {
                                break;
                            }
                            if c == '\u{1b}' && chars.peek() == Some(&'\\') {
                                chars.next();
                                break;
                            }
                        }
                    }
                    _ => {}
                },
                '\n' => result.push('\n'),
                '\t' => result.push_str("    "),
                c if c.is_control() => {}
                c => result.push(c),
            }
        }
        result
    }

    fn preview(mut message: Message) -> Message {
        message.role = Self::sanitize(&message.role)
            .graphemes(true)
            .take(256)
            .collect();
        message.text = Self::sanitize(&message.text);
        const OMITTED: &str = "\n\n[界面预览已省略，原文保留在会话记录]";
        if message.text.chars().count() > 16_000 {
            let budget = 16_000 - OMITTED.chars().count();
            let mut count = 0;
            message.text = message
                .text
                .graphemes(true)
                .take_while(|g| {
                    count += g.chars().count();
                    count <= budget
                })
                .collect();
            message.text.push_str(OMITTED);
        }
        message
    }
    fn trim_messages(&mut self) {
        let mut characters = self
            .messages
            .iter()
            .map(|m| m.text.chars().count() + m.role.chars().count())
            .sum::<usize>();
        while self.messages.len() > 150 || characters > 160_000 {
            let old = self.messages.remove(0);
            characters =
                characters.saturating_sub(old.text.chars().count() + old.role.chars().count());
            let removed_lines = markdown(&old.text, self.cache_width).len() + 2;
            self.conversation_scroll = self.conversation_scroll.saturating_sub(removed_lines);
            self.conversation_max = self.conversation_max.saturating_sub(removed_lines);
        }
    }
    pub fn push_message(&mut self, message: Message) {
        self.cache_dirty = true;
        self.messages.push(Self::preview(message));
        self.trim_messages();
    }

    pub fn push_activity(&mut self, mut activity: Activity) {
        activity.title = Self::sanitize(&activity.title);
        activity.detail = Self::sanitize(&activity.detail);
        let follow = self.activities.is_empty() || self.selected + 1 == self.activities.len();
        self.activities.push(activity);
        if follow {
            self.selected = self.activities.len() - 1;
        }
        if self.activities.len() > 400 {
            let removed = self.activities.len() - 400;
            self.activities.drain(..removed);
            self.selected = self.selected.saturating_sub(removed);
            self.activity_top = self.activity_top.saturating_sub(removed);
        }
    }

    pub fn history(&self) -> Vec<String> {
        self.history.clone()
    }
    pub fn set_history(&mut self, history: Vec<String>) {
        self.history = history
            .into_iter()
            .filter(|p| p.len() <= 128 * 1024)
            .map(|p| Self::sanitize(&p))
            .rev()
            .take(100)
            .collect();
        self.history.reverse();
        self.trim_history();
        self.history_index = None;
    }
    pub fn searching(&self) -> bool {
        self.search.is_some()
    }
    pub fn has_modal(&self) -> bool {
        self.searching() || self.picker.is_some() || self.show_help || self.detail.is_some()
    }
    pub fn start_search(&mut self, query: &str) {
        self.search = Some(Self::sanitize(query));
        self.search_selected = 0;
    }
    pub fn cursor(&self) -> usize {
        self.cursor
    }
    pub fn replace_range(&mut self, range: std::ops::Range<usize>, text: &str) {
        if range.start <= range.end
            && range.end <= self.draft.len()
            && self.draft.is_char_boundary(range.start)
            && self.draft.is_char_boundary(range.end)
            && (range.start == self.draft.len()
                || self
                    .draft
                    .grapheme_indices(true)
                    .any(|(i, _)| i == range.start))
            && (range.end == self.draft.len()
                || self
                    .draft
                    .grapheme_indices(true)
                    .any(|(i, _)| i == range.end))
        {
            let safe = Self::sanitize(text);
            if self.draft.len() - (range.end - range.start) + safe.len() > 128 * 1024 {
                self.notice = "输入最多 128 KiB；此次插入未接受，原输入已保留".into();
                return;
            }
            self.snapshot();
            self.draft.replace_range(range.clone(), &safe);
            self.cursor = range.start + safe.len();
            self.fix_cursor();
        }
    }
    fn snapshot(&mut self) {
        self.undo.push((self.draft.clone(), self.cursor));
        if self.undo.len() > 100 {
            self.undo.remove(0);
        }
        self.redo.clear();
    }
    fn trim_history(&mut self) {
        let mut bytes = self.history.iter().map(String::len).sum::<usize>();
        while self.history.len() > 100 || bytes > 1024 * 1024 {
            bytes -= self.history.remove(0).len();
        }
    }
    pub fn remember_prompt(&mut self, prompt: &str) {
        if prompt.len() <= 128 * 1024
            && !prompt.trim().is_empty()
            && self.history.last().map(String::as_str) != Some(prompt)
        {
            self.history.push(prompt.to_owned());
            self.trim_history();
        }
        self.history_index = None;
    }
    pub fn upsert_message(&mut self, message: Message) {
        self.cache_dirty = true;
        if let Some(index) = message.event_id.as_ref().and_then(|id| {
            self.messages
                .iter()
                .position(|m| m.event_id.as_ref() == Some(id))
        }) {
            self.messages[index] = Self::preview(message);
            self.trim_messages();
        } else {
            self.push_message(message);
        }
    }
    pub fn remove_message(&mut self, event_id: &str) {
        self.cache_dirty = true;
        self.messages
            .retain(|m| m.event_id.as_deref() != Some(event_id));
    }
    pub fn open_picker(&mut self, kind: PickerKind, items: Vec<PickerItem>, query: String) {
        self.picker = Some(Picker {
            kind,
            query,
            selected: 0,
            items,
        });
    }
    pub fn picker_value(&self) -> Option<(PickerKind, String)> {
        let p = self.picker.as_ref()?;
        filtered(p)
            .get(p.selected)
            .map(|item| (p.kind, item.value.clone()))
    }
    pub fn handle_mouse(&mut self, event: MouseEvent) {
        if self.show_help || self.searching() {
            return;
        }
        if let Some(picker) = self.picker.as_mut() {
            match event.kind {
                MouseEventKind::ScrollUp => picker.selected = picker.selected.saturating_sub(3),
                MouseEventKind::ScrollDown => {
                    picker.selected =
                        (picker.selected + 3).min(filtered(picker).len().saturating_sub(1))
                }
                _ => {}
            }
            return;
        }
        if self.detail.is_some() {
            match event.kind {
                MouseEventKind::ScrollUp => {
                    self.detail_scroll = self.detail_scroll.saturating_sub(3)
                }
                MouseEventKind::ScrollDown => {
                    self.detail_scroll = (self.detail_scroll + 3).min(self.detail_max)
                }
                _ => {}
            }
            return;
        }
        match event.kind {
            MouseEventKind::ScrollUp => {
                self.follow_conversation = false;
                self.conversation_scroll = self.conversation_scroll.saturating_sub(3);
            }
            MouseEventKind::ScrollDown => {
                self.conversation_scroll =
                    (self.conversation_scroll + 3).min(self.conversation_max);
                self.follow_conversation = self.conversation_scroll == self.conversation_max;
            }
            _ => {}
        }
    }
    fn vertical(&mut self, down: bool) {
        let start = self.draft[..self.cursor].rfind('\n').map_or(0, |p| p + 1);
        let end = self.draft[self.cursor..]
            .find('\n')
            .map_or(self.draft.len(), |p| self.cursor + p);
        let col = UnicodeWidthStr::width(&self.draft[start..self.cursor]);
        let target = if down && end < self.draft.len() {
            let next = end + 1;
            let last = self.draft[next..]
                .find('\n')
                .map_or(self.draft.len(), |p| next + p);
            Some((next, last))
        } else if !down && start > 0 {
            let last = start - 1;
            let first = self.draft[..last].rfind('\n').map_or(0, |p| p + 1);
            Some((first, last))
        } else {
            None
        };
        if let Some((first, last)) = target {
            let mut cells = 0;
            self.cursor = first;
            for (offset, g) in self.draft[first..last].grapheme_indices(true) {
                if cells + UnicodeWidthStr::width(g) > col {
                    break;
                }
                cells += UnicodeWidthStr::width(g);
                self.cursor = first + offset + g.len();
            }
        } else if !self.history.is_empty() {
            if self.history_index.is_none() {
                self.history_draft = self.draft.clone();
            }
            let index = if down {
                self.history_index
                    .map(|i| i + 1)
                    .unwrap_or(self.history.len())
            } else {
                self.history_index
                    .unwrap_or(self.history.len())
                    .saturating_sub(1)
            };
            if index >= self.history.len() {
                self.history_index = None;
                self.draft = self.history_draft.clone();
            } else {
                self.history_index = Some(index);
                self.draft = self.history[index].clone();
            }
            self.cursor = self.draft.len();
        }
    }
    fn line_start(&self) -> usize {
        self.draft[..self.cursor].rfind('\n').map_or(0, |i| i + 1)
    }
    fn line_end(&self) -> usize {
        self.draft[self.cursor..]
            .find('\n')
            .map_or(self.draft.len(), |i| self.cursor + i)
    }
    fn word_left(&self) -> usize {
        let mut start = self.cursor;
        for (i, g) in self.draft[..self.cursor].grapheme_indices(true).rev() {
            if !g.trim().is_empty() {
                start = i;
                break;
            }
            start = i;
        }
        for (i, g) in self.draft[..start].grapheme_indices(true).rev() {
            if g.trim().is_empty() {
                break;
            }
            start = i;
        }
        start
    }

    pub fn toggle_inspector(&mut self) {
        if self.terminal_width < 100 {
            self.narrow_activity = !self.narrow_activity;
            self.show_activity = true;
            if self.focus != Focus::Input {
                self.focus = if self.narrow_activity {
                    Focus::Activity
                } else {
                    Focus::Conversation
                };
            }
            return;
        }
        self.show_activity = !self.show_activity;
        if !self.show_activity && self.focus == Focus::Activity {
            self.focus = Focus::Input;
        }
    }
    pub fn handle_paste(&mut self, text: &str) {
        let query = Self::sanitize(text).replace('\n', " ");
        if let Some(picker) = self.picker.as_mut() {
            if picker.query.len() + query.len() <= 4096 {
                picker.query.push_str(&query);
                picker.selected = 0;
            } else {
                self.notice = "搜索输入最多 4 KiB；此次粘贴未接受".into();
            }
        } else if let Some(search) = self.search.as_mut() {
            if search.len() + query.len() <= 4096 {
                search.push_str(&query);
                self.search_selected = 0;
            } else {
                self.notice = "搜索输入最多 4 KiB；此次粘贴未接受".into();
            }
        } else if self.show_help || self.detail.is_some() {
            self.notice = "先按 Esc 返回输入，再粘贴到草稿".into();
        } else {
            self.paste(text);
        }
    }

    pub fn paste(&mut self, text: &str) {
        self.fix_cursor();
        // Normalize pasted line endings before stripping other terminal controls.
        let text = Self::sanitize(&text.replace("\r\n", "\n").replace('\r', "\n"));
        if self.draft.len() + text.len() > 128 * 1024 {
            self.notice = "输入最多 128 KiB；此次粘贴未接受，原输入已保留".into();
            return;
        }
        self.snapshot();
        self.draft.insert_str(self.cursor, &text);
        self.cursor += text.len();
        self.fix_cursor();
        self.focus = Focus::Input;
    }

    pub fn take_draft(&mut self) -> String {
        self.cursor = 0;
        self.undo.clear();
        self.redo.clear();
        std::mem::take(&mut self.draft)
    }

    pub fn selected_event(&self) -> Option<&str> {
        self.activities
            .get(self.selected)
            .map(|activity| activity.event_id.as_str())
    }

    fn fix_cursor(&mut self) {
        self.cursor = self.cursor.min(self.draft.len());
        if self.cursor != self.draft.len() {
            self.cursor = self
                .draft
                .grapheme_indices(true)
                .map(|(offset, _)| offset)
                .find(|&offset| offset >= self.cursor)
                .unwrap_or(self.draft.len());
        }
    }

    fn previous(&self) -> usize {
        self.draft
            .grapheme_indices(true)
            .map(|(offset, _)| offset)
            .take_while(|&offset| offset < self.cursor)
            .last()
            .unwrap_or(0)
    }

    fn next(&self) -> usize {
        self.draft
            .grapheme_indices(true)
            .map(|(offset, _)| offset)
            .find(|&offset| offset > self.cursor)
            .unwrap_or(self.draft.len())
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        if let Some(p) = self.picker.as_mut() {
            match key.code {
                KeyCode::Esc => self.picker = None,
                KeyCode::Up => p.selected = p.selected.saturating_sub(1),
                KeyCode::Down => {
                    p.selected = (p.selected + 1).min(filtered(p).len().saturating_sub(1))
                }
                KeyCode::PageUp => p.selected = p.selected.saturating_sub(8),
                KeyCode::PageDown => {
                    p.selected = (p.selected + 8).min(filtered(p).len().saturating_sub(1))
                }
                KeyCode::Backspace => {
                    p.query.pop();
                    p.selected = 0;
                }
                KeyCode::Char(c)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                        && p.query.len() + c.len_utf8() <= 4096 =>
                {
                    p.query.push(c);
                    p.selected = 0;
                }
                _ => {}
            }
            return;
        }
        if key.code == KeyCode::Char('f') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.search = Some(String::new());
            self.search_selected = 0;
            return;
        }
        if let Some(query) = self.search.as_mut() {
            match key.code {
                KeyCode::Esc => self.search = None,
                KeyCode::Backspace => {
                    query.pop();
                    self.search_selected = 0;
                }
                KeyCode::Char(c)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    if query.len() + c.len_utf8() <= 4096 {
                        query.push(c);
                        self.search_selected = 0;
                    }
                }
                KeyCode::Enter | KeyCode::Down => self.search_selected += 1,
                KeyCode::Up => self.search_selected = self.search_selected.saturating_sub(1),
                _ => {}
            }
            return;
        }
        self.fix_cursor();
        if key.code == KeyCode::F(1) {
            self.show_help = !self.show_help;
            return;
        }
        if key.code == KeyCode::Esc {
            self.show_help = false;
            self.detail = None;
            self.detail_scroll = 0;
            self.focus = Focus::Input;
            return;
        }
        if self.show_help {
            return;
        }
        if self.detail.is_some() {
            scroll_key(key.code, &mut self.detail_scroll, self.detail_max, 10);
            return;
        }
        match key.code {
            KeyCode::F(2) => {
                self.toggle_inspector();
                return;
            }
            KeyCode::Tab | KeyCode::BackTab => {
                if self.terminal_width < 100 {
                    self.focus = if self.focus == Focus::Input {
                        if self.narrow_activity {
                            Focus::Activity
                        } else {
                            Focus::Conversation
                        }
                    } else {
                        Focus::Input
                    };
                    return;
                }
                self.focus = match self.focus {
                    Focus::Input => Focus::Conversation,
                    Focus::Conversation if self.show_activity => Focus::Activity,
                    _ => Focus::Input,
                };
                return;
            }
            _ => {}
        }
        match self.focus {
            Focus::Input => match key.code {
                KeyCode::Char('z') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if let Some(old) = self.undo.pop() {
                        self.redo.push((self.draft.clone(), self.cursor));
                        (self.draft, self.cursor) = old;
                    }
                }
                KeyCode::Char('z') if key.modifiers.contains(KeyModifiers::ALT) => {
                    if let Some(next) = self.redo.pop() {
                        self.undo.push((self.draft.clone(), self.cursor));
                        (self.draft, self.cursor) = next;
                    }
                }
                KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.cursor = self.line_start()
                }
                KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.cursor = self.line_end()
                }
                KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.cursor = self.previous()
                }
                KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.cursor = self.next()
                }
                KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.replace_range(self.word_left()..self.cursor, "");
                }
                KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    let end = self.line_end();
                    self.replace_range(self.cursor..end, "");
                }
                KeyCode::Left if key.modifiers.contains(KeyModifiers::ALT) => {
                    self.cursor = self.word_left()
                }
                KeyCode::Right if key.modifiers.contains(KeyModifiers::ALT) => {
                    while self.cursor < self.draft.len()
                        && !self.draft[self.cursor..].starts_with(char::is_whitespace)
                    {
                        self.cursor = self.next();
                    }
                    while self.cursor < self.draft.len()
                        && self.draft[self.cursor..].starts_with(char::is_whitespace)
                    {
                        self.cursor = self.next();
                    }
                }
                KeyCode::Up => self.vertical(false),
                KeyCode::Down => self.vertical(true),
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.replace_range(self.line_start()..self.cursor, "");
                }
                KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.paste("\n")
                }
                KeyCode::Enter
                    if key.modifiers.intersects(
                        KeyModifiers::ALT | KeyModifiers::SHIFT | KeyModifiers::CONTROL,
                    ) =>
                {
                    self.paste("\n")
                }
                KeyCode::Char(c)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    self.paste(&c.to_string())
                }
                KeyCode::Left => self.cursor = self.previous(),
                KeyCode::Right => self.cursor = self.next(),
                KeyCode::Home => self.cursor = self.line_start(),
                KeyCode::End => self.cursor = self.line_end(),
                KeyCode::Backspace => {
                    self.replace_range(self.previous()..self.cursor, "");
                }
                KeyCode::Delete => {
                    self.replace_range(self.cursor..self.next(), "");
                }
                KeyCode::PageUp => {
                    if self.terminal_width < 100 && self.narrow_activity {
                        self.focus = Focus::Activity;
                        self.selected = self.selected.saturating_sub(self.activity_height.max(1));
                        return;
                    }
                    self.focus = Focus::Conversation;
                    self.follow_conversation = false;
                    self.conversation_scroll = self.conversation_scroll.saturating_sub(10);
                }
                KeyCode::PageDown => {
                    if self.terminal_width < 100 && self.narrow_activity {
                        self.focus = Focus::Activity;
                        self.selected = (self.selected + self.activity_height.max(1))
                            .min(self.activities.len().saturating_sub(1));
                        return;
                    }
                    self.focus = Focus::Conversation;
                    self.conversation_scroll =
                        (self.conversation_scroll + 10).min(self.conversation_max);
                    self.follow_conversation = self.conversation_scroll == self.conversation_max;
                }
                _ => {}
            },
            Focus::Conversation => {
                if scroll_key(
                    key.code,
                    &mut self.conversation_scroll,
                    self.conversation_max,
                    10,
                ) {
                    self.follow_conversation = self.conversation_scroll == self.conversation_max;
                }
            }
            Focus::Activity => {
                scroll_key(
                    key.code,
                    &mut self.selected,
                    self.activities.len().saturating_sub(1),
                    self.activity_height.max(1),
                );
            }
        }
    }

    pub fn render(
        &mut self,
        frame: &mut Frame,
        state: &bone::state::SessionState,
        model: &str,
        status: &str,
    ) {
        let area = frame.area();
        if area.width < 100 && self.terminal_width >= 100 && self.focus == Focus::Activity {
            self.narrow_activity = true;
        }
        self.terminal_width = area.width;
        frame.render_widget(
            Block::default().style(Style::default().bg(BACKGROUND).fg(TEXT)),
            area,
        );
        if area.width < 8 || area.height < 6 {
            frame.render_widget(
                Paragraph::new("请扩大终端窗口").style(Style::default().fg(ACCENT)),
                area,
            );
            return;
        }
        self.fix_cursor();
        let input_width = area.width.saturating_sub(2).max(1) as usize;
        let (draft_lines, cursor_row, cursor_col) =
            wrap_with_cursor(&Self::sanitize(&self.draft), self.cursor, input_width);
        let input_height = (draft_lines.len() + 2)
            .clamp(3, 7)
            .min(area.height.saturating_sub(5) as usize) as u16;
        let regions = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(1),
                Constraint::Length(input_height),
                Constraint::Length(2),
            ])
            .split(area);
        let header = format!(
            " BONE {} · {} · {}",
            if self.busy {
                ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"]
                    [((self.started.elapsed().as_millis() / 120) % 8) as usize]
            } else {
                ""
            },
            Self::sanitize(model),
            Self::sanitize(status)
        );
        frame.render_widget(
            Paragraph::new(vec![Line::styled(
                header,
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            )]),
            regions[0],
        );
        let panels = if self.show_activity && area.width >= 100 {
            Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Min(40),
                    Constraint::Length((area.width / 3).min(48)),
                ])
                .split(regions[1])
                .to_vec()
        } else {
            vec![regions[1]]
        };
        if area.width < 100 && self.narrow_activity {
            self.render_activity(frame, panels[0]);
        } else {
            let conversation = panels[0];
            let inner = conversation.inner(ratatui::layout::Margin {
                horizontal: 1,
                vertical: 0,
            });
            let width = inner.width.max(1) as usize;
            if self.cache_dirty || self.cache_width != width {
                let mut lines = Vec::new();
                if self.messages.is_empty() {
                    lines.push(Line::styled(
                        "描述任务，BONE 会在当前工作区执行。",
                        Style::default().fg(ACCENT),
                    ));
                    lines.push(Line::styled(
                        "/ 命令   @ 文件   Ctrl+F 搜索   F1 快捷键",
                        Style::default().fg(MUTED),
                    ));
                }
                for message in &self.messages {
                    let tool = message.role.starts_with("工具");
                    let streaming = message.role.contains("输出中");
                    lines.push(Line::styled(
                        message.role.clone(),
                        Style::default()
                            .fg(if tool || streaming { MUTED } else { ACCENT })
                            .add_modifier(Modifier::BOLD),
                    ));
                    lines.extend(markdown(&message.text, width));
                    lines.push(Line::from(""));
                }
                self.transcript_cache = lines;
                self.cache_width = width;
                self.cache_dirty = false;
            }
            let mut search_lines = None;
            let mut matches = Vec::new();
            if self.search.as_ref().is_some_and(|q| !q.is_empty()) {
                search_lines = Some(self.transcript_cache.clone());
            }
            let lines = search_lines.as_mut().unwrap_or(&mut self.transcript_cache);
            if let Some(query) = self.search.as_ref().filter(|q| !q.is_empty()) {
                for (index, line) in lines.iter_mut().enumerate() {
                    let text = line
                        .spans
                        .iter()
                        .map(|s| s.content.as_ref())
                        .collect::<String>();
                    if text.to_lowercase().contains(&query.to_lowercase()) {
                        matches.push(index);
                        *line = line
                            .clone()
                            .style(Style::default().add_modifier(Modifier::REVERSED));
                    }
                }
                if !matches.is_empty() {
                    self.search_selected %= matches.len();
                    self.conversation_scroll = matches[self.search_selected];
                    self.follow_conversation = false;
                }
            }
            self.search_total = matches.len();
            self.conversation_max = lines.len().saturating_sub(inner.height as usize);
            if self.follow_conversation {
                self.conversation_scroll = self.conversation_max;
            }
            self.conversation_scroll = self.conversation_scroll.min(self.conversation_max);

            frame.render_widget(
                Paragraph::new(
                    lines
                        .iter()
                        .skip(self.conversation_scroll)
                        .take(inner.height as usize)
                        .cloned()
                        .collect::<Vec<_>>(),
                ),
                inner,
            );
            if let Some(&activity_area) = panels.get(1) {
                self.render_activity(frame, activity_area);
            }
        }
        frame.render_widget(
            block(
                " 输入 · Enter 发送 · Shift+Enter 换行 ",
                self.focus == Focus::Input,
            ),
            regions[2],
        );
        let input_inner = regions[2].inner(ratatui::layout::Margin {
            horizontal: 1,
            vertical: 1,
        });
        let input_top = cursor_row.saturating_sub(input_inner.height.saturating_sub(1) as usize);
        let visible = draft_lines
            .into_iter()
            .skip(input_top)
            .take(input_inner.height as usize)
            .map(Line::from)
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(visible), input_inner);
        if self.focus == Focus::Input
            && !self.show_help
            && self.detail.is_none()
            && self.picker.is_none()
            && self.search.is_none()
            && input_inner.height > 0
        {
            frame.set_cursor_position((
                input_inner.x + cursor_col.min(input_inner.width.saturating_sub(1) as usize) as u16,
                input_inner.y + (cursor_row - input_top) as u16,
            ));
        }
        let footer = if let Some(query) = &self.search {
            format!(
                " 搜索: {} · {}/{} · ↑↓ / Enter 下一个 · Esc 关闭",
                query,
                if self.search_total == 0 {
                    0
                } else {
                    self.search_selected + 1
                },
                self.search_total
            )
        } else {
            format!(
                " {} · {} · {} · {} · {}",
                state
                    .workspace
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy(),
                Self::sanitize(model),
                Self::sanitize(&self.session_label),
                Self::sanitize(&self.usage),
                if self.live_status.is_empty() {
                    Self::sanitize(status)
                } else {
                    Self::sanitize(&self.live_status)
                }
            )
        };
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled(
                    if self.notice.is_empty() {
                        " Ctrl+P 命令 · Ctrl+O 文件 · Ctrl+G 编辑 · F1 帮助".into()
                    } else {
                        format!(" {}", Self::sanitize(&self.notice).replace('\n', " "))
                    },
                    Style::default().fg(ACCENT),
                ),
                Line::styled(footer, Style::default().fg(MUTED)),
            ]),
            regions[3],
        );
        if let Some(picker) = &self.picker {
            let rect = overlay_rect(area);
            frame.render_widget(Clear, rect);
            frame.render_widget(
                block(
                    &format!(" {:?} · {} · Esc 关闭 ", picker.kind, picker.query),
                    true,
                ),
                rect,
            );
            let inner = rect.inner(ratatui::layout::Margin {
                horizontal: 1,
                vertical: 1,
            });
            let items = filtered(picker);
            let top = picker
                .selected
                .saturating_sub(inner.height.saturating_sub(1) as usize);
            let rows = items
                .iter()
                .enumerate()
                .skip(top)
                .take(inner.height as usize)
                .map(|(i, item)| {
                    Line::styled(
                        format!(
                            "{} {}  {}",
                            if i == picker.selected { "›" } else { " " },
                            Self::sanitize(&item.label),
                            Self::sanitize(&item.detail)
                        ),
                        if i == picker.selected {
                            Style::default().add_modifier(Modifier::REVERSED)
                        } else {
                            Style::default()
                        },
                    )
                })
                .collect::<Vec<_>>();
            frame.render_widget(
                Paragraph::new(if rows.is_empty() {
                    vec![Line::from("没有匹配项")]
                } else {
                    rows
                }),
                inner,
            );
        } else if self.show_help {
            let help = "直接输入任务，Enter 发送。\nShift+Enter / Alt+Enter / Ctrl+J 换行；粘贴多行保持在输入框。\n↑↓ 移动行光标，首尾行召回历史；Alt+←→ 移动单词。\nCtrl+A/E 行首尾；Ctrl+U/K 删除至行首尾；Ctrl+W 删除单词。\nCtrl+Z 撤销 / Alt+Z 重做；Ctrl+F 搜索对话。\nCtrl+P 命令菜单；Ctrl+O / @文件 Tab 引用；Ctrl+G 外部编辑器。\nTab 切换焦点；窄屏仅在输入与当前主面板间切换。Esc 返回输入。\n对话：上下 / PageUp / PageDown 滚动，Home / End 到首尾。\n对话展示最近最多 150 条、约 16 万字符；单条预览最多 1.6 万字符。\n界面预览省略的历史和原文保留在会话记录。\n活动展示最近 400 条：上下选择，Enter 浏览原生事件详情。\nF2：宽屏显示或隐藏活动；窄屏切换对话与活动主面板。\nCtrl+C 暂停行动；Ctrl+R 恢复；Ctrl+Q 退出。\n\nF1 或 Esc 关闭帮助。";
            let commands = super::COMMANDS
                .iter()
                .map(|(name, _)| *name)
                .collect::<Vec<_>>()
                .join(" ");
            render_overlay(
                frame,
                area,
                " 帮助 ",
                &format!("{help}\n\n{commands}"),
                &mut 0,
            );
        } else if let Some((title, detail)) = &self.detail {
            self.detail_max = render_overlay(
                frame,
                area,
                &format!(" {} · Esc 关闭 ", Self::sanitize(title)),
                detail,
                &mut self.detail_scroll,
            );
        } else {
            self.detail_scroll = 0;
        }
        if std::env::var_os("NO_COLOR").is_some()
            || std::env::var("TERM").is_ok_and(|term| term == "dumb")
        {
            for cell in &mut frame.buffer_mut().content {
                cell.fg = Color::Reset;
                cell.bg = Color::Reset;
            }
        }
    }

    fn render_activity(&mut self, frame: &mut Frame, area: Rect) {
        frame.render_widget(
            block(
                " 活动 · 只读 · 最近 400 · Enter 详情 ",
                self.focus == Focus::Activity,
            ),
            area,
        );
        let inner = area.inner(ratatui::layout::Margin {
            horizontal: 1,
            vertical: 1,
        });
        let preview_height = if inner.height >= 7 {
            (inner.height / 3).clamp(3, 5)
        } else {
            0
        };
        let regions = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(preview_height)])
            .split(inner);
        let list_area = regions[0];
        self.activity_height = list_area.height as usize;
        self.selected = self.selected.min(self.activities.len().saturating_sub(1));
        if self.selected < self.activity_top {
            self.activity_top = self.selected;
        }
        if self.selected >= self.activity_top + self.activity_height {
            self.activity_top = self
                .selected
                .saturating_sub(self.activity_height.saturating_sub(1));
        }
        let lines = self
            .activities
            .iter()
            .enumerate()
            .skip(self.activity_top)
            .take(self.activity_height)
            .map(|(index, activity)| {
                let selected = index == self.selected;
                let color = match activity.tone {
                    Tone::Normal => TEXT,
                    Tone::Info => ACCENT,
                    Tone::Success => Color::Rgb(126, 205, 160),
                    Tone::Error => Color::Rgb(242, 141, 141),
                };
                let style = if selected {
                    Style::default().fg(color).bg(Color::Rgb(36, 49, 65))
                } else {
                    Style::default().fg(color)
                };
                Line::from(Span::styled(
                    format!(
                        "{} {}",
                        if selected { "›" } else { " " },
                        Self::sanitize(&activity.title).replace('\n', " ")
                    ),
                    style,
                ))
            })
            .collect::<Vec<_>>();
        if lines.is_empty() {
            frame.render_widget(
                Paragraph::new("等待行动记录…").style(Style::default().fg(MUTED)),
                list_area,
            );
        } else {
            frame.render_widget(Paragraph::new(lines), list_area);
        }
        if preview_height > 0 {
            let mut preview = vec![Line::styled(
                "─ 选中活动预览 · Enter 详情 ─",
                Style::default().fg(MUTED),
            )];
            if let Some(activity) = self.activities.get(self.selected) {
                preview.extend(
                    wrap(
                        &Self::sanitize(&activity.detail),
                        inner.width.max(1) as usize,
                    )
                    .into_iter()
                    .take(preview_height.saturating_sub(1) as usize)
                    .map(Line::from),
                );
            }
            frame.render_widget(Paragraph::new(preview), regions[1]);
        }
    }
}

fn scroll_key(key: KeyCode, position: &mut usize, max: usize, page: usize) -> bool {
    *position = match key {
        KeyCode::Up => position.saturating_sub(1),
        KeyCode::Down => (*position + 1).min(max),
        KeyCode::PageUp => position.saturating_sub(page),
        KeyCode::PageDown => (*position + page).min(max),
        KeyCode::Home => 0,
        KeyCode::End => max,
        _ => return false,
    };
    true
}

fn filtered(picker: &Picker) -> Vec<&PickerItem> {
    let query = picker.query.to_lowercase();
    picker
        .items
        .iter()
        .filter(|item| {
            let haystack = format!("{} {}", item.label, item.detail).to_lowercase();
            let mut chars = haystack.chars();
            query
                .chars()
                .all(|needle| chars.by_ref().any(|c| c == needle))
        })
        .collect()
}
fn markdown(text: &str, width: usize) -> Vec<Line<'static>> {
    let mut result = Vec::new();
    let mut code = false;
    let mut diff = false;
    for raw in text.split('\n') {
        if let Some(language) = raw.strip_prefix("```") {
            code = !code;
            diff = code && language.trim() == "diff";
            result.push(Line::styled(
                if code {
                    format!("  {}", language.trim())
                } else {
                    String::new()
                },
                Style::default().fg(MUTED),
            ));
            continue;
        }
        let style = if (diff || !code) && raw.starts_with('+') {
            Style::default().fg(Color::Green)
        } else if (diff || !code) && raw.starts_with('-') && !raw.starts_with("- ") {
            Style::default().fg(Color::Red)
        } else if raw.starts_with("@@") {
            Style::default().fg(ACCENT)
        } else if !code && raw.starts_with('#') {
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
        } else if code {
            Style::default().fg(MUTED)
        } else {
            Style::default()
        };
        let rendered = if !code && raw.starts_with('#') {
            raw.trim_start_matches('#').trim_start().to_owned()
        } else if code {
            format!("  {raw}")
        } else {
            raw.to_owned()
        };
        for line in wrap(&rendered, width) {
            if code {
                result.push(Line::styled(line, style));
            } else {
                let mut inline = false;
                let spans = line
                    .split('`')
                    .map(|part| {
                        let current = inline;
                        inline = !inline;
                        Span::styled(
                            part.to_owned(),
                            if current {
                                style.add_modifier(Modifier::BOLD)
                            } else {
                                style
                            },
                        )
                    })
                    .collect::<Vec<_>>();
                result.push(Line::from(spans));
            }
        }
    }
    result
}

fn block(title: &str, focused: bool) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(if focused {
            ACCENT
        } else {
            Color::Rgb(54, 66, 82)
        }))
}

fn overlay_rect(area: Rect) -> Rect {
    let inset_x = (area.width / 12).min(8);
    let inset_y = (area.height / 8).min(3);
    area.inner(ratatui::layout::Margin {
        horizontal: inset_x,
        vertical: inset_y,
    })
}

fn render_overlay(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    text: &str,
    scroll: &mut usize,
) -> usize {
    let rect = overlay_rect(area);
    frame.render_widget(Clear, rect);
    frame.render_widget(
        block(title, true).style(Style::default().bg(BACKGROUND).fg(TEXT)),
        rect,
    );
    let inner = rect.inner(ratatui::layout::Margin {
        horizontal: 1,
        vertical: 1,
    });
    let content = markdown(&View::sanitize(text), inner.width.max(1) as usize);
    let max = content.len().saturating_sub(inner.height as usize);
    *scroll = (*scroll).min(max);
    let lines = content
        .into_iter()
        .skip(*scroll)
        .take(inner.height as usize)
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(BACKGROUND).fg(TEXT)),
        inner,
    );
    max
}

fn wrap(text: &str, width: usize) -> Vec<String> {
    wrap_with_cursor(text, text.len(), width).0
}

/// Soft-wrap by display cells while retaining grapheme boundaries and an insertion cursor.
fn wrap_with_cursor(text: &str, cursor: usize, width: usize) -> (Vec<String>, usize, usize) {
    let width = width.max(1);
    let mut lines = vec![String::new()];
    let mut column = 0;
    let mut position = (0, 0);
    for (offset, grapheme) in text.grapheme_indices(true) {
        let cells = UnicodeWidthStr::width(grapheme);
        if grapheme != "\n" && column + cells > width && column > 0 {
            lines.push(String::new());
            column = 0;
        }
        if offset == cursor {
            position = (lines.len() - 1, column);
        }
        if grapheme == "\n" {
            lines.push(String::new());
            column = 0;
        } else {
            lines.last_mut().unwrap().push_str(grapheme);
            column += cells;
            if column >= width {
                lines.push(String::new());
                column = 0;
            }
        }
    }
    if cursor >= text.len() {
        position = (lines.len() - 1, column);
    }
    (lines, position.0, position.1)
}

#[cfg(test)]
#[path = "../../tests/unit/tui_view.rs"]
mod tests;
