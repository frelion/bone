use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};
use ratatui_textarea::{CursorMove, TextArea, WrapMode};
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
    History,
    Question,
    Reconcile,
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
    pub inline: bool,
}

#[derive(Debug)]
pub(super) struct View {
    editor: TextArea<'static>,
    pub reply_label: String,
    message_selected: Option<usize>,
    expanded: std::collections::HashSet<String>,
    message_offsets: Vec<usize>,
    selection_needs_scroll: bool,
    pub usage: String,
    pub session_label: String,
    pub busy: bool,
    pub live_status: String,
    pub picker: Option<Picker>,
    history: Vec<String>,
    history_index: Option<usize>,
    history_draft: String,
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
            editor: new_editor(),
            reply_label: String::new(),
            message_selected: None,
            expanded: Default::default(),
            message_offsets: Vec::new(),
            selection_needs_scroll: false,
            usage: String::new(),
            session_label: String::new(),
            busy: false,
            live_status: String::new(),
            picker: None,
            history: Vec::new(),
            history_index: None,
            history_draft: String::new(),
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
        const OMITTED: &str = "\n\n[显示预览，d 原文 / y 复制完整记录]";
        if message.text.len() > 128 * 1024 {
            let budget = 128 * 1024 - OMITTED.len();
            let end = message
                .text
                .grapheme_indices(true)
                .map(|(i, g)| i + g.len())
                .take_while(|&end| end <= budget)
                .last()
                .unwrap_or(0);
            message.text.truncate(end);
            message.text.push_str(OMITTED);
        }
        message
    }
    pub fn push_message(&mut self, message: Message) {
        self.cache_dirty = true;
        self.messages.push(Self::preview(message));
        self.trim_messages(true);
    }
    pub fn first_message_id(&self) -> Option<&str> {
        self.messages.first()?.event_id.as_deref()
    }
    fn trim_messages(&mut self, from_head: bool) {
        let bytes =
            |m: &Message| m.role.len() + m.text.len() + m.event_id.as_ref().map_or(0, String::len);
        let mut total = self.messages.iter().map(bytes).sum::<usize>();
        while self.messages.len() > 256 || total > 8 * 1024 * 1024 {
            let index = if from_head {
                0
            } else {
                self.messages.len() - 1
            };
            let removed = self.messages.remove(index);
            total -= bytes(&removed);
            let key = message_key(&removed);
            if from_head {
                let height = message_lines(
                    &removed,
                    self.cache_width,
                    self.expanded.contains(&key),
                    false,
                )
                .len();
                self.conversation_scroll = self.conversation_scroll.saturating_sub(height);
            }
            self.expanded.remove(&key);
            self.message_selected = match self.message_selected {
                Some(selected) if selected == index => {
                    self.notice = "所选消息已移出预览窗口；Ctrl+F 搜索持久原文".into();
                    self.selection_needs_scroll = false;
                    None
                }
                Some(selected) if selected > index => Some(selected - 1),
                selected => selected,
            };
        }
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
    pub fn has_modal(&self) -> bool {
        self.picker.as_ref().is_some_and(|p| !p.inline) || self.show_help || self.detail.is_some()
    }
    pub fn draft(&self) -> String {
        self.editor.lines().join("\n")
    }
    pub fn cursor(&self) -> usize {
        let ratatui_textarea::DataCursor(row, col) = self.editor.cursor();
        self.editor
            .lines()
            .iter()
            .take(row)
            .map(|s| s.len() + 1)
            .sum::<usize>()
            + self.editor.lines()[row]
                .chars()
                .take(col)
                .map(char::len_utf8)
                .sum::<usize>()
    }
    fn move_to_byte(&mut self, offset: usize) {
        let draft = self.draft();
        let prefix = &draft[..offset];
        let row = prefix.bytes().filter(|&b| b == b'\n').count();
        let col = prefix
            .rsplit('\n')
            .next()
            .unwrap_or_default()
            .chars()
            .count();
        self.editor.move_cursor(CursorMove::Jump(
            row.min(u16::MAX as usize) as u16,
            col.min(u16::MAX as usize) as u16,
        ));
        for _ in u16::MAX as usize..row {
            self.editor.move_cursor(CursorMove::Down);
        }
        for _ in u16::MAX as usize..col {
            self.editor.move_cursor(CursorMove::Forward);
        }
    }
    pub fn selected_input_text(&self) -> Option<String> {
        if self.focus != Focus::Input || self.has_modal() {
            return None;
        }
        let ((a, b), (c, d)) = self.editor.selection_range()?;
        if (a, b) == (c, d) {
            return None;
        }
        let mut lines = Vec::new();
        for row in a..=c {
            lines.push(
                self.editor.lines()[row]
                    .chars()
                    .skip(if row == a { b } else { 0 })
                    .take(if row == c {
                        d - if row == a { b } else { 0 }
                    } else {
                        usize::MAX
                    })
                    .collect::<String>(),
            );
        }
        Some(lines.join("\n"))
    }
    pub fn replace_range(&mut self, range: std::ops::Range<usize>, text: &str) {
        let draft = self.draft();
        let boundary = |i| i == draft.len() || draft.grapheme_indices(true).any(|(p, _)| p == i);
        if range.start > range.end
            || range.end > draft.len()
            || !boundary(range.start)
            || !boundary(range.end)
        {
            return;
        }
        let safe = Self::sanitize(text);
        if draft.len() - range.len() + safe.len() > 128 * 1024 {
            self.notice = "输入最多 128 KiB；此次插入未接受，原输入已保留".into();
            return;
        }
        self.editor.cancel_selection();
        self.move_to_byte(range.start);
        if !range.is_empty() {
            self.editor.start_selection();
            self.move_to_byte(range.end);
        }
        if safe.is_empty() {
            if !range.is_empty() {
                self.editor.delete_char();
            }
        } else {
            self.editor.insert_str(safe);
        }
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
            self.trim_messages(true);
        } else {
            self.push_message(message);
        }
    }
    pub fn remove_message(&mut self, event_id: &str) {
        self.cache_dirty = true;
        let selected_id = self.selected_message_id().map(str::to_owned);
        self.messages
            .retain(|m| m.event_id.as_deref() != Some(event_id));
        self.message_selected = selected_id.as_deref().and_then(|id| {
            self.messages
                .iter()
                .position(|m| m.event_id.as_deref() == Some(id))
        });
    }
    pub fn open_picker(&mut self, kind: PickerKind, items: Vec<PickerItem>, query: String) {
        self.picker = Some(Picker {
            kind,
            query,
            selected: 0,
            items,
            inline: false,
        });
    }
    pub fn open_completion(&mut self, kind: PickerKind, items: Vec<PickerItem>, query: String) {
        let selected = self
            .picker
            .as_ref()
            .filter(|p| p.inline && p.kind == kind)
            .map_or(0, |p| p.selected);
        self.open_picker(kind, items, query);
        let picker = self.picker.as_mut().unwrap();
        picker.inline = true;
        picker.selected = selected.min(filtered(picker).len().saturating_sub(1));
    }
    pub fn is_completion(&self) -> bool {
        self.picker.as_ref().is_some_and(|p| p.inline)
    }
    pub fn close_completion(&mut self) {
        if self.is_completion() {
            self.picker = None;
        }
    }
    pub fn selected_message(&self) -> Option<&Message> {
        self.messages.get(self.message_selected?)
    }
    pub fn selected_message_id(&self) -> Option<&str> {
        self.selected_message()?.event_id.as_deref()
    }
    pub fn select_message(&mut self, id: &str) -> bool {
        let Some(index) = self
            .messages
            .iter()
            .position(|m| m.event_id.as_deref() == Some(id))
        else {
            return false;
        };
        self.message_selected = Some(index);
        self.selection_needs_scroll = true;
        self.focus = Focus::Conversation;
        self.follow_conversation = false;
        self.cache_dirty = true;
        true
    }
    pub fn toggle_selected_message(&mut self) {
        if let Some(message) = self.selected_message() {
            let key = self
                .selected_message_id()
                .map(str::to_owned)
                .unwrap_or_else(|| message_key(message));
            if !self.expanded.remove(&key) {
                self.expanded.insert(key);
            }
            self.cache_dirty = true;
        }
    }
    pub fn prepend_messages(&mut self, messages: Vec<Message>) {
        let existing = self
            .messages
            .iter()
            .filter_map(|m| m.event_id.clone())
            .collect::<std::collections::HashSet<_>>();
        let mut older = messages
            .into_iter()
            .filter(|m| m.event_id.as_ref().is_none_or(|id| !existing.contains(id)))
            .map(Self::preview)
            .collect::<Vec<_>>();
        let shift = older
            .iter()
            .map(|m| {
                message_lines(
                    m,
                    self.cache_width,
                    self.expanded.contains(&message_key(m)),
                    false,
                )
                .len()
            })
            .sum::<usize>();
        if !self.follow_conversation {
            self.conversation_scroll += shift;
        }
        if let Some(index) = self.message_selected.as_mut() {
            *index += older.len();
        }
        older.append(&mut self.messages);
        self.messages = older;
        self.trim_messages(false);
        self.cache_dirty = true;
    }
    pub fn picker_value(&self) -> Option<(PickerKind, String)> {
        let p = self.picker.as_ref()?;
        filtered(p)
            .get(p.selected)
            .map(|item| (p.kind, item.value.clone()))
    }
    pub fn handle_mouse(&mut self, event: MouseEvent) {
        if self.show_help {
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
    fn recall_history(&mut self, down: bool) {
        if self.history.is_empty() {
            return;
        }
        if self.history_index.is_none() {
            self.history_draft = self.draft();
        }
        let index = if down {
            self.history_index.map_or(self.history.len(), |i| i + 1)
        } else {
            self.history_index
                .unwrap_or(self.history.len())
                .saturating_sub(1)
        };
        let text = if index >= self.history.len() {
            self.history_index = None;
            self.history_draft.clone()
        } else {
            self.history_index = Some(index);
            self.history[index].clone()
        };
        self.editor = new_editor();
        self.editor.insert_str(text);
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
        if let Some(picker) = self.picker.as_mut().filter(|p| !p.inline) {
            if picker.query.len() + query.len() <= 4096 {
                picker.query.push_str(&query);
                picker.selected = 0;
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
        let text = Self::sanitize(&text.replace("\r\n", "\n").replace('\r', "\n"));
        let selected = self.selected_input_text().map_or(0, |s| s.len());
        if self.draft().len() - selected + text.len() > 128 * 1024 {
            self.notice = "输入最多 128 KiB；此次粘贴未接受，原输入已保留".into();
            return;
        }
        self.editor.insert_str(text);
        self.focus = Focus::Input;
    }
    pub fn take_draft(&mut self) -> String {
        let draft = self.draft();
        self.editor = new_editor();
        draft
    }

    pub fn selected_event(&self) -> Option<&str> {
        self.activities
            .get(self.selected)
            .map(|activity| activity.event_id.as_str())
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        if let Some(p) = self.picker.as_mut().filter(|p| {
            !p.inline
                || matches!(
                    key.code,
                    KeyCode::Esc
                        | KeyCode::Up
                        | KeyCode::Down
                        | KeyCode::PageUp
                        | KeyCode::PageDown
                )
        }) {
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
                } else {
                    self.focus = match self.focus {
                        Focus::Input => Focus::Conversation,
                        Focus::Conversation if self.show_activity => Focus::Activity,
                        _ => Focus::Input,
                    };
                }
                if self.focus == Focus::Conversation
                    && self.message_selected.is_none()
                    && !self.messages.is_empty()
                {
                    self.message_selected = Some(self.messages.len() - 1);
                    self.selection_needs_scroll = true;
                    self.cache_dirty = true;
                }
                return;
            }
            _ => {}
        }
        match self.focus {
            Focus::Input => {
                match key.code {
                    KeyCode::Enter if key.modifiers.is_empty() => {}
                    KeyCode::Enter | KeyCode::Char('j')
                        if key.code == KeyCode::Enter
                            || key.modifiers.contains(KeyModifiers::CONTROL) =>
                    {
                        self.paste("\n")
                    }
                    KeyCode::Up | KeyCode::Down if key.modifiers.contains(KeyModifiers::ALT) => {
                        self.recall_history(key.code == KeyCode::Down)
                    }
                    KeyCode::Char('z') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.editor.undo();
                    }
                    KeyCode::Char('z') if key.modifiers.contains(KeyModifiers::ALT) => {
                        self.editor.redo();
                    }
                    KeyCode::PageUp | KeyCode::PageDown => {
                        self.focus = if self.terminal_width < 100 && self.narrow_activity {
                            Focus::Activity
                        } else {
                            Focus::Conversation
                        };
                        self.handle_key(key);
                    }
                    KeyCode::Left | KeyCode::Right | KeyCode::Backspace | KeyCode::Delete
                        if key.modifiers.is_empty()
                            || (key.modifiers == KeyModifiers::SHIFT
                                && matches!(key.code, KeyCode::Left | KeyCode::Right)) =>
                    {
                        // textarea uses character columns. Keep deletion/navigation atomic for emoji and combining clusters.
                        let draft = self.draft();
                        let cursor = self.cursor();
                        let previous = draft
                            .grapheme_indices(true)
                            .map(|(i, _)| i)
                            .take_while(|&i| i < cursor)
                            .last()
                            .unwrap_or(0);
                        let next = draft
                            .grapheme_indices(true)
                            .map(|(i, _)| i)
                            .find(|&i| i > cursor)
                            .unwrap_or(draft.len());
                        if self.selected_input_text().is_some()
                            && matches!(key.code, KeyCode::Backspace | KeyCode::Delete)
                        {
                            self.editor.delete_char();
                        } else {
                            match key.code {
                                KeyCode::Left => {
                                    if key.modifiers.contains(KeyModifiers::SHIFT) {
                                        if self.editor.selection_range().is_none() {
                                            self.editor.start_selection();
                                        }
                                    } else {
                                        self.editor.cancel_selection();
                                    }
                                    self.move_to_byte(previous);
                                }
                                KeyCode::Right => {
                                    if key.modifiers.contains(KeyModifiers::SHIFT) {
                                        if self.editor.selection_range().is_none() {
                                            self.editor.start_selection();
                                        }
                                    } else {
                                        self.editor.cancel_selection();
                                    }
                                    self.move_to_byte(next);
                                }
                                KeyCode::Backspace if previous < cursor => {
                                    self.replace_range(previous..cursor, "")
                                }
                                KeyCode::Delete if cursor < next => {
                                    self.replace_range(cursor..next, "")
                                }
                                _ => {}
                            }
                        }
                    }
                    KeyCode::Char(c)
                        if !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                    {
                        self.paste(&c.to_string())
                    }
                    _ => {
                        self.editor.input(key);
                        let draft = self.draft();
                        let cursor = self.cursor();
                        if cursor < draft.len()
                            && !draft.grapheme_indices(true).any(|(i, _)| i == cursor)
                        {
                            let boundary = draft
                                .grapheme_indices(true)
                                .map(|(i, _)| i)
                                .find(|&i| i > cursor)
                                .unwrap_or(draft.len());
                            self.move_to_byte(boundary);
                        }
                    }
                }
            }
            Focus::Conversation => {
                if matches!(
                    key.code,
                    KeyCode::Up | KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('k')
                ) {
                    if !self.messages.is_empty() {
                        let current = self.message_selected.unwrap_or(self.messages.len() - 1);
                        self.message_selected =
                            Some(if matches!(key.code, KeyCode::Down | KeyCode::Char('j')) {
                                (current + 1).min(self.messages.len() - 1)
                            } else {
                                current.saturating_sub(1)
                            });
                        self.follow_conversation = false;
                        self.cache_dirty = true;
                        self.selection_needs_scroll = true;
                    }
                } else if key.code == KeyCode::Enter {
                    self.toggle_selected_message();
                } else if scroll_key(
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
        let input_width = area.width.saturating_sub(2).max(1) as usize;
        let draft_lines = wrap(&self.draft(), input_width);
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
                let anchor = if self.cache_width != width && !self.follow_conversation {
                    self.message_offsets
                        .iter()
                        .enumerate()
                        .rev()
                        .find(|(_, offset)| **offset <= self.conversation_scroll)
                        .map(|(index, offset)| (index, self.conversation_scroll - offset))
                } else {
                    None
                };
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
                self.message_offsets.clear();
                for (index, message) in self.messages.iter().enumerate() {
                    self.message_offsets.push(lines.len());
                    lines.extend(message_lines(
                        message,
                        width,
                        self.expanded.contains(&message_key(message)),
                        self.message_selected == Some(index),
                    ));
                }
                if let Some((index, offset)) = anchor
                    && let Some(&start) = self.message_offsets.get(index)
                {
                    let end = self
                        .message_offsets
                        .get(index + 1)
                        .copied()
                        .unwrap_or(lines.len());
                    self.conversation_scroll = start + offset.min(end.saturating_sub(start + 1));
                }
                if self.selection_needs_scroll {
                    if let Some(index) = self.message_selected
                        && let Some(&offset) = self.message_offsets.get(index)
                    {
                        self.conversation_scroll = offset;
                    }
                    self.selection_needs_scroll = false;
                }
                self.transcript_cache = lines;
                self.cache_width = width;
                self.cache_dirty = false;
            }
            let lines = &self.transcript_cache;
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
        let title = if self.reply_label.is_empty() {
            " 输入 · Enter 发送 · Shift+Enter 换行 ".to_owned()
        } else {
            format!(" 输入 · {} ", Self::sanitize(&self.reply_label))
        };
        self.editor.set_block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_style(Style::default().fg(if self.focus == Focus::Input {
                    ACCENT
                } else {
                    MUTED
                })),
        );
        self.editor
            .set_cursor_style(if self.focus == Focus::Input && !self.has_modal() {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            });
        frame.render_widget(&self.editor, regions[2]);
        let footer = {
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
            let rect = if picker.inline {
                let height = (filtered(picker).len().clamp(1, 6) + 2) as u16;
                Rect::new(
                    regions[2].x,
                    regions[2].y.saturating_sub(height),
                    regions[2].width,
                    height.min(regions[2].y.saturating_sub(area.y)),
                )
            } else {
                overlay_rect(area)
            };
            frame.render_widget(Clear, rect);
            frame.render_widget(
                block(
                    &format!(
                        " {} · {} · {} ",
                        picker_title(picker.kind),
                        picker.query,
                        if picker.inline {
                            "↑↓ 选择 · Tab 插入 · Esc 关闭"
                        } else {
                            "Enter 选择 · Esc 关闭"
                        }
                    ),
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
            let help = "直接输入任务，Enter 发送。\nShift+Enter / Alt+Enter / Ctrl+J 换行；粘贴多行保持在输入框。\n↑↓ 移动光标；Alt+↑↓ 召回历史；Ctrl+←→ 移动单词。\nCtrl+A/E 行首尾；Ctrl+K 删除至行尾；Ctrl+W 删除单词。\nShift+方向键选区；Ctrl+Y复制选区或当前原文。\nCtrl+Z 撤销 / Alt+Z 重做；Ctrl+F 搜索对话。\nCtrl+P 命令菜单；Ctrl+O / @文件 Tab 引用；Ctrl+G 外部编辑器。\nTab 切换焦点；窄屏仅在输入与当前主面板间切换。Esc 返回输入。\n对话：↑↓ / j k 选择消息；Enter 展开折叠；d 原文；y 复制。\nPageUp / PageDown 滚动，Home / End 到首尾。\n长消息默认折叠；窗口最多256条/8MiB，单条128KiB预览。\n展开预览；d 原文 / y 复制完整记录；Ctrl+F 搜索持久历史。\n活动展示最近 400 条：上下选择，Enter 浏览原生事件详情。\nF2：宽屏显示或隐藏活动；窄屏切换对话与活动主面板。\nCtrl+C：有输入选区时复制，否则暂停；Ctrl+R 恢复；Ctrl+Q 退出。\n\nF1 或 Esc 关闭帮助。";
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
    if picker.kind == PickerKind::History {
        return picker.items.iter().collect();
    }
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
fn new_editor() -> TextArea<'static> {
    let mut editor = TextArea::default();
    editor.set_wrap_mode(WrapMode::Glyph);
    editor.set_max_histories(100);
    editor.set_cursor_line_style(Style::default());
    editor.set_selection_style(Style::default().bg(Color::DarkGray));
    editor.set_placeholder_text("描述任务，或输入 / 命令、@ 文件");
    editor.set_placeholder_style(Style::default().fg(MUTED));
    editor
}
fn picker_title(kind: PickerKind) -> &'static str {
    match kind {
        PickerKind::Command => "命令",
        PickerKind::Session => "会话",
        PickerKind::File => "文件",
        PickerKind::Model => "模型",
        PickerKind::History => "搜索历史",
        PickerKind::Question => "问题",
        PickerKind::Reconcile => "核对变更",
    }
}
fn message_key(message: &Message) -> String {
    message.event_id.clone().unwrap_or_else(|| {
        use std::hash::{Hash, Hasher};
        let mut hash = std::hash::DefaultHasher::new();
        message.role.hash(&mut hash);
        message.text.hash(&mut hash);
        format!("anonymous:{:x}", hash.finish())
    })
}
fn message_lines(
    message: &Message,
    width: usize,
    expanded: bool,
    selected: bool,
) -> Vec<Line<'static>> {
    let tool = message.role.starts_with("工具");
    let status = if message.role.contains("失败")
        || message.role.contains("错误")
        || (tool
            && message
                .text
                .lines()
                .next()
                .is_some_and(|s| s.starts_with("失败") || s.starts_with("错误")))
    {
        Color::Red
    } else if tool
        && message.text.lines().next().is_some_and(|s| {
            s.starts_with("结果未知") || s.starts_with("未知") || s.starts_with("待核对")
        })
    {
        Color::Yellow
    } else if message.role.contains("输出中")
        || message.role.contains("运行")
        || (tool
            && message
                .text
                .lines()
                .next()
                .is_some_and(|s| s.starts_with("执行中") || s.starts_with("运行中")))
    {
        Color::Blue
    } else if tool
        && (message.role.contains("完成")
            || message
                .text
                .lines()
                .next()
                .is_some_and(|s| s.starts_with("完成")))
    {
        Color::Green
    } else if tool {
        Color::Yellow
    } else {
        ACCENT
    };
    let mut body = if tool {
        wrap(&message.text, width)
            .into_iter()
            .map(|s| Line::styled(s, Style::default().fg(MUTED)))
            .collect::<Vec<_>>()
    } else {
        markdown(&message.text, width)
    };
    let limit = if tool { 6 } else { 20 };
    let folded = !expanded && body.len() > limit;
    let mut result = vec![Line::styled(
        format!(
            "{} {}{}",
            if selected { "›" } else { " " },
            message.role,
            if folded { " · Enter 展开" } else { "" }
        ),
        Style::default().fg(status).add_modifier(Modifier::BOLD),
    )];
    if folded {
        let omitted = body.len() - limit;
        body.truncate(limit);
        body.push(Line::styled(
            format!("… 还有 {omitted} 行 · Enter 展开 · d 原文"),
            Style::default().fg(MUTED),
        ));
    }
    result.extend(body);
    result.push(Line::from(""));
    result
}
fn markdown(text: &str, width: usize) -> Vec<Line<'static>> {
    let parsed = tui_markdown::from_str(text);
    let mut result = Vec::new();
    for line in parsed.lines {
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut cells = 0;
        for span in line.spans {
            for g in span.content.graphemes(true) {
                let size = UnicodeWidthStr::width(g);
                if cells + size > width.max(1) && cells > 0 {
                    result.push(Line::from(std::mem::take(&mut spans)).style(line.style));
                    cells = 0;
                }
                if let Some(last) = spans.last_mut().filter(|s| s.style == span.style) {
                    last.content.to_mut().push_str(g);
                } else {
                    spans.push(Span::styled(g.to_owned(), span.style));
                }
                cells += size;
            }
        }
        result.push(Line::from(spans).style(line.style));
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
    let content = wrap(&View::sanitize(text), inner.width.max(1) as usize)
        .into_iter()
        .map(Line::from)
        .collect::<Vec<_>>();
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
