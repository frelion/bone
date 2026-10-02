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
    conversation_width: usize,
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
            cache_width: 0,
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
            conversation_width: 80,
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
            let removed_lines = markdown(&old.text, self.conversation_width).len() + 2;
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
    pub fn insert_at_cursor(&mut self, text: &str) {
        self.paste(text);
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
            match key.code {
                KeyCode::Up => self.detail_scroll = self.detail_scroll.saturating_sub(1),
                KeyCode::Down => self.detail_scroll = (self.detail_scroll + 1).min(self.detail_max),
                KeyCode::PageUp => self.detail_scroll = self.detail_scroll.saturating_sub(10),
                KeyCode::PageDown => {
                    self.detail_scroll = (self.detail_scroll + 10).min(self.detail_max)
                }
                KeyCode::Home => self.detail_scroll = 0,
                KeyCode::End => self.detail_scroll = self.detail_max,
                _ => {}
            }
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
                    self.cursor = self.draft[..self.cursor].rfind('\n').map_or(0, |i| i + 1)
                }
                KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.cursor = self.draft[self.cursor..]
                        .find('\n')
                        .map_or(self.draft.len(), |i| self.cursor + i)
                }
                KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.cursor = self.previous()
                }
                KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.cursor = self.next()
                }
                KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    let start = self.word_left();
                    self.snapshot();
                    self.draft.replace_range(start..self.cursor, "");
                    self.cursor = start;
                }
                KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    let end = self.draft[self.cursor..]
                        .find('\n')
                        .map_or(self.draft.len(), |i| self.cursor + i);
                    self.snapshot();
                    self.draft.replace_range(self.cursor..end, "");
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
                    self.snapshot();
                    let start = self.draft[..self.cursor].rfind('\n').map_or(0, |i| i + 1);
                    self.draft.replace_range(start..self.cursor, "");
                    self.cursor = start;
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
                KeyCode::Home => {
                    self.cursor = self.draft[..self.cursor].rfind('\n').map_or(0, |i| i + 1)
                }
                KeyCode::End => {
                    self.cursor = self.draft[self.cursor..]
                        .find('\n')
                        .map_or(self.draft.len(), |i| self.cursor + i)
                }
                KeyCode::Backspace => {
                    self.snapshot();
                    let start = self.previous();
                    self.draft.replace_range(start..self.cursor, "");
                    self.cursor = start;
                }
                KeyCode::Delete => {
                    self.snapshot();
                    let end = self.next();
                    self.draft.replace_range(self.cursor..end, "");
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
                match key.code {
                    KeyCode::Up => {
                        self.conversation_scroll = self.conversation_scroll.saturating_sub(1)
                    }
                    KeyCode::Down => {
                        self.conversation_scroll =
                            (self.conversation_scroll + 1).min(self.conversation_max)
                    }
                    KeyCode::PageUp => {
                        self.conversation_scroll = self.conversation_scroll.saturating_sub(10)
                    }
                    KeyCode::PageDown => {
                        self.conversation_scroll =
                            (self.conversation_scroll + 10).min(self.conversation_max)
                    }
                    KeyCode::Home => self.conversation_scroll = 0,
                    KeyCode::End => self.conversation_scroll = self.conversation_max,
                    _ => return,
                }
                self.follow_conversation = self.conversation_scroll == self.conversation_max;
            }
            Focus::Activity => {
                let max = self.activities.len().saturating_sub(1);
                match key.code {
                    KeyCode::Up => self.selected = self.selected.saturating_sub(1),
                    KeyCode::Down => self.selected = (self.selected + 1).min(max),
                    KeyCode::PageUp => {
                        self.selected = self.selected.saturating_sub(self.activity_height.max(1))
                    }
                    KeyCode::PageDown => {
                        self.selected = (self.selected + self.activity_height.max(1)).min(max)
                    }
                    KeyCode::Home => self.selected = 0,
                    KeyCode::End => self.selected = max,
                    _ => {}
                }
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
            self.conversation_width = inner.width.max(1) as usize;
            if self.cache_dirty || self.cache_width != self.conversation_width {
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
                    lines.extend(markdown(&message.text, self.conversation_width));
                    lines.push(Line::from(""));
                }
                self.transcript_cache = lines;
                self.cache_width = self.conversation_width;
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
            let help = "直接输入任务，Enter 发送。\nShift+Enter / Alt+Enter / Ctrl+J 换行；粘贴多行保持在输入框。\n↑↓ 移动行光标，首尾行召回历史；Alt+←→ 移动单词。\nCtrl+A/E 行首尾；Ctrl+U/K 删除至行首尾；Ctrl+W 删除单词。\nCtrl+Z 撤销 / Alt+Z 重做；Ctrl+F 搜索对话。\nCtrl+P 命令菜单；Ctrl+O / @文件 Tab 引用；Ctrl+G 外部编辑器。\n/sessions 会话；/model 模型；/status 用量与限制。\n/diff 项目修改；/export 导出；/older 更早原文。\nTab 切换焦点；窄屏仅在输入与当前主面板间切换。Esc 返回输入。\n对话：上下 / PageUp / PageDown 滚动，Home / End 到首尾。\n对话展示最近最多 150 条、约 16 万字符；单条预览最多 1.6 万字符。\n界面预览省略的历史和原文保留在会话记录。\n活动展示最近 400 条：上下选择，Enter 浏览原生事件详情。\nF2：宽屏显示或隐藏活动；窄屏切换对话与活动主面板。\nCtrl+C 暂停行动；Ctrl+R 恢复；Ctrl+Q 退出。\n\nF1 或 Esc 关闭帮助。";
            render_overlay(frame, area, " 帮助 ", help, 0);
        } else if let Some((title, detail)) = &self.detail {
            let overlay = overlay_rect(area);
            let content = markdown(
                &Self::sanitize(detail),
                overlay.width.saturating_sub(2).max(1) as usize,
            );
            self.detail_max = content
                .len()
                .saturating_sub(overlay.height.saturating_sub(2) as usize);
            self.detail_scroll = self.detail_scroll.min(self.detail_max);
            render_overlay(
                frame,
                area,
                &format!(" {} · Esc 关闭 ", Self::sanitize(title)),
                detail,
                self.detail_scroll,
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

fn render_overlay(frame: &mut Frame, area: Rect, title: &str, text: &str, scroll: usize) {
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
    let lines = markdown(&View::sanitize(text), inner.width.max(1) as usize)
        .into_iter()
        .skip(scroll)
        .take(inner.height as usize)
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(BACKGROUND).fg(TEXT)),
        inner,
    );
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
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn render_fixture_snapshots() {
        let Ok(directory) = std::env::var("BONE_TUI_SNAPSHOT_DIR") else {
            return;
        };
        use ratatui::{Terminal, backend::TestBackend};
        let directory = std::path::Path::new(&directory);
        std::fs::create_dir_all(directory).unwrap();
        fn escape(text: &str) -> String {
            text.replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
                .replace('"', "&quot;")
        }
        fn color(color: Color, fallback: &str) -> String {
            match color {
                Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
                Color::Cyan | Color::LightCyan => "#78c9d4".into(),
                Color::Green | Color::LightGreen => "#93c58c".into(),
                Color::Red | Color::LightRed => "#ed9292".into(),
                Color::DarkGray => "#9a9da5".into(),
                Color::Yellow | Color::LightYellow => "#e1c589".into(),
                _ => fallback.into(),
            }
        }
        for width in [80, 120] {
            let height = 36;
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let state = bone::state::SessionState::new("/fixture/BONE");
            let mut view = View::new();
            view.session_label = "fixture".into();
            view.usage = "2 / 24 calls · 1,240 tokens · 可写".into();
            view.live_status = "工作中 · 12s".into();
            view.notice = "渲染测试数据 · 非真实模型运行 · Ctrl+P 命令 · Ctrl+O 文件".into();
            for (role, text, id) in [
                (
                    "你",
                    "给这个 CLI 增加错误恢复，并保留 Unicode 输入 👩‍💻。",
                    "fixture-user",
                ),
                (
                    "Agent · 进度",
                    "## 实现计划\n- 检查状态持久化与输入边界\n- 修复失败恢复，并运行回归测试\n\n修改集中在 `src/tui/view.rs`。",
                    "fixture-plan",
                ),
                (
                    "工具 · read_file",
                    "完成 · src/tui/view.rs\n读取输入编辑与恢复流程。",
                    "fixture-tool",
                ),
                (
                    "工具 · apply_patch",
                    "完成 · 输入恢复\n```diff\n@@ recover_draft\n- draft.clear();\n+ draft = previous.clone();\n```",
                    "fixture-diff",
                ),
                (
                    "Agent · 输出中（未交付）",
                    "边界处理已完成，正在验证多行粘贴与组合字符。\n这段是未交付的流式预览。",
                    "fixture-live",
                ),
            ] {
                view.push_message(Message {
                    role: role.into(),
                    text: text.into(),
                    event_id: Some(id.into()),
                });
            }
            view.paste("补充：失败时保留当前草稿。\n请验证中文、emoji 和多行输入。");
            terminal
                .draw(|f| view.render(f, &state, "provider/model", "工作中"))
                .unwrap();
            let buffer = terminal.backend().buffer();
            let mut plain =
                String::from("BONE TUI render fixture — synthetic data, not a real model run\n");
            let mut svg = format!(
                "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{}\" height=\"{}\" viewBox=\"0 0 {} {}\"><title>BONE TUI render fixture — synthetic data, not a real model run</title><rect width=\"100%\" height=\"100%\" fill=\"#111318\"/><text x=\"10\" y=\"20\" fill=\"#a0a4ad\" font-size=\"12\" font-family=\"monospace\">Render fixture · synthetic data · {} columns</text><g font-family=\"'SFMono-Regular', 'Noto Sans Mono CJK SC', monospace\" font-size=\"14\">",
                width * 9 + 20,
                height * 20 + 40,
                width * 9 + 20,
                height * 20 + 40,
                width
            );
            for y in 0..height {
                let mut skip = 0;
                for x in 0..width {
                    let cell = &buffer[(x, y)];
                    if skip > 0 {
                        skip -= 1;
                        continue;
                    }
                    let symbol = cell.symbol();
                    plain.push_str(symbol);
                    let cells = UnicodeWidthStr::width(symbol).max(1);
                    skip = cells.saturating_sub(1);
                    let reversed = cell.modifier.contains(Modifier::REVERSED);
                    let foreground = color(
                        if reversed { cell.bg } else { cell.fg },
                        if reversed { "#111318" } else { "#dcdfe5" },
                    );
                    let background = color(
                        if reversed { cell.fg } else { cell.bg },
                        if reversed { "#dcdfe5" } else { "#111318" },
                    );
                    if background != "#111318" {
                        svg.push_str(&format!(
                            "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"20\" fill=\"{}\"/>",
                            x * 9 + 10,
                            y * 20 + 28,
                            cells * 9,
                            background
                        ));
                    }
                    if symbol != " " {
                        svg.push_str(&format!(
                            "<text x=\"{}\" y=\"{}\" fill=\"{}\"{}>{}</text>",
                            x * 9 + 10,
                            y * 20 + 44,
                            foreground,
                            if cell.modifier.contains(Modifier::BOLD) {
                                " font-weight=\"bold\""
                            } else {
                                ""
                            },
                            escape(symbol)
                        ));
                    }
                }
                plain.push('\n');
            }
            svg.push_str("</g></svg>");
            std::fs::write(directory.join(format!("preview-{width}.txt")), plain).unwrap();
            std::fs::write(directory.join(format!("preview-{width}.svg")), svg).unwrap();
        }
    }

    #[test]
    fn modal_paste_targets_query_and_never_mutates_hidden_draft() {
        let mut view = View::new();
        view.paste("草稿");
        view.start_search("find");
        view.handle_paste("👩‍💻\n中");
        assert_eq!(view.search.as_deref(), Some("find👩‍💻 中"));
        assert_eq!(view.draft, "草稿");
        view.handle_paste(&"x".repeat(4096));
        assert_eq!(view.search.as_deref(), Some("find👩‍💻 中"));
        view.handle_key(key(KeyCode::Esc));
        view.detail = Some(("详情".into(), "body".into()));
        view.handle_paste("hidden");
        assert_eq!(view.draft, "草稿");
    }

    #[test]
    fn oversized_paste_preserves_draft_cursor_and_undo() {
        let mut view = View::new();
        view.paste("原文👩‍💻");
        view.handle_key(key(KeyCode::Left));
        let before = (view.draft.clone(), view.cursor, view.undo.len());
        view.paste(&"中".repeat(50_000));
        assert_eq!((view.draft.clone(), view.cursor, view.undo.len()), before);
        assert!(view.notice.contains("128 KiB"));
        view.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
        assert!(view.draft.contains('\n'));
    }
    #[test]
    fn stream_updates_cannot_exceed_history_preview_bounds() {
        let mut view = View::new();
        for i in 0..20 {
            view.push_message(Message {
                role: "AI".into(),
                text: "small".into(),
                event_id: Some(i.to_string()),
            });
        }
        for i in 0..20 {
            view.upsert_message(Message {
                role: "AI".into(),
                text: "e\u{301}".repeat(20_000),
                event_id: Some(i.to_string()),
            });
        }
        assert!(
            view.messages
                .iter()
                .map(|m| m.text.chars().count() + m.role.chars().count())
                .sum::<usize>()
                <= 160_000
        );
        for message in &view.messages {
            assert!(message.text.chars().count() <= 16_000);
            assert!(!message.text.ends_with('e'));
        }
        for i in 0..20 {
            view.remember_prompt(&format!("{i}{}", "中".repeat(30_000)));
        }
        assert!(view.history.iter().map(String::len).sum::<usize>() <= 1024 * 1024);
    }

    #[test]
    fn picker_filters_selects_and_closes_without_editing_prompt() {
        let mut view = View::new();
        view.paste("任务");
        view.open_picker(
            PickerKind::Command,
            vec![
                PickerItem {
                    label: "Sessions".into(),
                    detail: "Resume history".into(),
                    value: "/resume".into(),
                },
                PickerItem {
                    label: "Model".into(),
                    detail: String::new(),
                    value: "/model".into(),
                },
            ],
            "ssn".into(),
        );
        assert_eq!(
            view.picker_value(),
            Some((PickerKind::Command, "/resume".into()))
        );
        view.handle_key(key(KeyCode::Esc));
        assert!(view.picker.is_none());
        assert_eq!(view.draft, "任务");
    }
    #[test]
    fn multiline_editor_history_and_undo_preserve_unicode() {
        let mut view = View::new();
        view.paste("中文\n👩‍💻a");
        view.handle_key(key(KeyCode::Up));
        assert_eq!(view.cursor, "中".len());
        view.handle_key(key(KeyCode::Down));
        assert_eq!(view.cursor, "中文\n👩‍💻".len());
        view.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        view.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));
        assert_eq!(view.draft, "中文\n👩‍💻a");
        view.take_draft();
        view.remember_prompt("旧任务");
        view.handle_key(key(KeyCode::Up));
        assert_eq!(view.draft, "旧任务");
        view.handle_key(key(KeyCode::Down));
        assert_eq!(view.draft, "");
    }
    #[test]
    fn markdown_styles_diff_and_bounds_cells() {
        let lines = markdown("# 标题\n`代码`\n```diff\n+新增\n-删除\n@@ chunk\n```", 8);
        assert!(lines.iter().any(|l| l.style.fg == Some(Color::Green)
            || l.spans.iter().any(|s| s.style.fg == Some(Color::Green))));
        assert!(lines.iter().any(|l| l.style.fg == Some(Color::Red)
            || l.spans.iter().any(|s| s.style.fg == Some(Color::Red))));
        for line in lines {
            assert!(line.width() <= 8);
        }
    }
    #[test]
    fn transcript_cache_invalidates_and_search_is_explicit() {
        let state = bone::state::SessionState::new("/tmp/work");
        use ratatui::{Terminal, backend::TestBackend};
        let mut view = View::new();
        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
        view.upsert_message(Message {
            role: "AI".into(),
            text: "needle".into(),
            event_id: Some("1".into()),
        });
        view.start_search("needle");
        terminal
            .draw(|f| view.render(f, &state, "m", "idle"))
            .unwrap();
        assert!(!view.cache_dirty);
        assert!(view.search.is_some());
        view.handle_key(key(KeyCode::Esc));
        assert!(view.search.is_none());
        view.remove_message("1");
        assert!(view.messages.is_empty());
        assert!(view.cache_dirty);
    }

    #[test]
    fn editing_preserves_graphemes_and_wide_characters() {
        let mut view = View::new();
        view.paste("中文👩‍💻e\u{301}");
        view.handle_key(key(KeyCode::Backspace));
        assert_eq!(view.draft, "中文👩‍💻");
        view.handle_key(key(KeyCode::Left));
        view.handle_key(key(KeyCode::Delete));
        assert_eq!(view.draft, "中文");
        view.handle_key(key(KeyCode::Left));
        view.paste("你好");
        assert_eq!(view.draft, "中你好文");
    }

    #[test]
    fn pasted_lines_and_enter_do_not_submit() {
        let mut view = View::new();
        view.paste("第一行\r\n第二行\r第三行");
        view.handle_key(key(KeyCode::Enter));
        assert_eq!(view.draft, "第一行\n第二行\n第三行");
        assert_eq!(view.take_draft(), "第一行\n第二行\n第三行");
        assert!(view.draft.is_empty());
    }

    #[test]
    fn strips_terminal_sequences_and_controls() {
        let safe = View::sanitize("中文\x1b[31m红\x1b[0m\x07\r\x1b]52;c;secret\x07\n\t好\u{85}");
        assert_eq!(safe, "中文红\n    好");
        assert!(safe.chars().all(|c| !c.is_control() || c == '\n'));
    }

    #[test]
    fn cursor_uses_cells_after_wrapping() {
        let (lines, row, col) = wrap_with_cursor("中文a", "中文a".len(), 4);
        assert_eq!(lines, ["中文", "a"]);
        assert_eq!((row, col), (1, 1));
        let (_, row, col) = wrap_with_cursor("e\u{301}👩‍💻", "e\u{301}👩‍💻".len(), 8);
        assert_eq!((row, col), (0, 3));
    }

    #[test]
    fn scrolled_markdown_eviction_keeps_the_visible_message() {
        let mut view = View::new();
        view.conversation_width = 8;
        view.push_message(Message {
            role: "Agent".into(),
            text: "```text\nabcdefgh\n```".into(),
            event_id: None,
        });
        for index in 1..150 {
            view.push_message(Message {
                role: "Agent".into(),
                text: format!("item {index}"),
                event_id: None,
            });
        }
        // Two extra lines frame each message. Code indentation wraps its body
        // onto two lines, so the first message occupies six rendered lines.
        view.follow_conversation = false;
        view.conversation_scroll = 9;
        view.conversation_max = 450;
        view.push_message(Message {
            role: "Agent".into(),
            text: "item 150".into(),
            event_id: None,
        });
        assert_eq!(view.messages[0].text, "item 1");
        assert_eq!(view.conversation_scroll, 3);
        assert_eq!(view.conversation_max, 444);
        assert!(!view.follow_conversation);
    }

    #[test]
    fn activity_eviction_retains_selected_event() {
        let mut view = View::new();
        for index in 0..400 {
            view.push_activity(Activity {
                event_id: index.to_string(),
                title: "行动".into(),
                detail: String::new(),
                tone: Tone::Normal,
            });
        }
        view.selected = 100;
        view.activity_top = 90;
        view.push_activity(Activity {
            event_id: "400".into(),
            title: "行动".into(),
            detail: String::new(),
            tone: Tone::Normal,
        });
        assert_eq!(view.activities.len(), 400);
        assert_eq!(view.selected_event(), Some("100"));
        assert_eq!(view.activity_top, 89);
    }

    #[test]
    fn message_preview_limits_are_unicode_safe_and_bounded() {
        let mut view = View::new();
        for index in 0..160 {
            view.push_message(Message {
                role: "用户".into(),
                text: "中文".into(),
                event_id: Some(index.to_string()),
            });
        }
        assert_eq!(view.messages.len(), 150);
        assert_eq!(
            view.messages.first().unwrap().event_id.as_deref(),
            Some("10")
        );
        for _ in 0..12 {
            view.push_message(Message {
                role: "AI".into(),
                text: "\x1b[31m中".repeat(20_000),
                event_id: None,
            });
        }
        let text = &view.messages.last().unwrap().text;
        assert_eq!(text.chars().count(), 16_000);
        assert!(text.ends_with("[界面预览已省略，原文保留在会话记录]"));
        assert!(
            view.messages
                .iter()
                .map(|m| m.text.chars().count() + m.role.chars().count())
                .sum::<usize>()
                <= 160_000
        );
    }

    #[test]
    fn renders_narrow_wide_and_modal_views_without_terminal_controls() {
        let state = bone::state::SessionState::new("/tmp/中文\x07workspace");
        use ratatui::{Terminal, backend::TestBackend};
        for (width, height) in [(120, 30), (80, 20), (8, 6), (4, 3)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let mut view = View::new();
            view.paste("中文\n👩‍💻\n第三行");
            view.push_message(Message {
                role: "用户".into(),
                text: "\x1b[31m任务\x07详情".into(),
                event_id: None,
            });
            view.push_activity(Activity {
                event_id: "event-1".into(),
                title: "工具完成".into(),
                detail: "原生事件".into(),
                tone: Tone::Success,
            });
            view.notice = "状态\r\x07正常".into();
            terminal
                .draw(|frame| view.render(frame, &state, "model\x1b[0m", "idle\x07"))
                .unwrap();
            for cell in terminal.backend().buffer().content() {
                assert!(cell.symbol().chars().all(|c| !c.is_control()));
            }
            if width == 80 {
                view.handle_key(key(KeyCode::F(2)));
                view.handle_key(key(KeyCode::Tab));
                terminal
                    .draw(|frame| view.render(frame, &state, "model", "idle"))
                    .unwrap();
                assert_eq!(view.focus, Focus::Activity);
                assert_eq!(view.selected_event(), Some("event-1"));
                let text = terminal
                    .backend()
                    .buffer()
                    .content()
                    .iter()
                    .map(|cell| cell.symbol())
                    .collect::<String>();
                assert!(text.replace(' ', "").contains("原生事件"));
                view.handle_key(key(KeyCode::Tab));
                assert_eq!(view.focus, Focus::Input);
                view.handle_key(key(KeyCode::F(2)));
                view.handle_key(key(KeyCode::Tab));
                assert_eq!(view.focus, Focus::Conversation);
            }
            view.show_help = true;
            terminal
                .draw(|frame| view.render(frame, &state, "model", "idle"))
                .unwrap();
            view.show_help = false;
            view.detail = Some(("事件".into(), "详情\n".repeat(40)));
            terminal
                .draw(|frame| view.render(frame, &state, "model", "idle"))
                .unwrap();
        }
    }
}
