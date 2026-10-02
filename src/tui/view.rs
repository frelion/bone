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
    pub summary: Option<String>,
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

#[derive(Debug, Clone)]
struct ReadPoint {
    key: String,
    offset: usize,
    duplicate: usize,
}

#[derive(Debug, Clone)]
struct LayerReturn {
    picker: Option<Picker>,
    detail: Option<(String, String)>,
    help: bool,
    audit: bool,
    focus: Focus,
    scroll: usize,
    width: usize,
    reading_scroll: usize,
    following: bool,
    read_anchor: Option<ReadPoint>,
    read_excerpt: String,
    audit_event: Option<String>,
    audit_top_event: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) struct Draft {
    editor: TextArea<'static>,
    label: String,
}

#[derive(Debug)]
struct TemporaryDraft {
    draft: Draft,
    layers: Vec<LayerReturn>,
    parent: LayerReturn,
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
    layers: Vec<LayerReturn>,
    temporary_draft: Option<TemporaryDraft>,
    read_points: Vec<ReadPoint>,
    pending_anchor: Option<ReadPoint>,
    anchor_excerpt: String,
    unread: usize,
    detail_width: usize,
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
            layers: Vec::new(),
            temporary_draft: None,
            read_points: Vec::new(),
            pending_anchor: None,
            anchor_excerpt: String::new(),
            unread: 0,
            detail_width: 0,
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
        message.summary = message.summary.map(|s| Self::sanitize(&s));
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
    fn retain_reading_anchor(&mut self) {
        if !self.follow_conversation && self.pending_anchor.is_none() {
            self.pending_anchor = self.read_points.get(self.conversation_scroll).cloned();
            if let Some(anchor) = &self.pending_anchor
                && let Some(message) = self
                    .messages
                    .iter()
                    .find(|message| message_key(message) == anchor.key)
            {
                self.anchor_excerpt = message
                    .text
                    .get(anchor.offset..)
                    .unwrap_or_default()
                    .graphemes(true)
                    .take(24)
                    .collect();
            }
        }
    }
    pub fn push_message(&mut self, message: Message) {
        self.retain_reading_anchor();
        if !self.follow_conversation {
            self.unread += 1;
        }
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
        self.retain_reading_anchor();
        self.cache_dirty = true;
        if let Some(index) = message.event_id.as_ref().and_then(|id| {
            self.messages
                .iter()
                .position(|m| m.event_id.as_ref() == Some(id))
        }) {
            if !self.follow_conversation && self.messages[index].text != message.text {
                self.unread += 1;
            }
            self.messages[index] = Self::preview(message);
            self.trim_messages(true);
        } else {
            self.push_message(message);
        }
    }
    pub fn remove_message(&mut self, event_id: &str) {
        self.retain_reading_anchor();
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
    fn enter_layer(&mut self) {
        self.retain_reading_anchor();
        self.layers.push(LayerReturn {
            picker: self.picker.take().filter(|p| !p.inline),
            detail: self.detail.take(),
            help: self.show_help,
            audit: self.show_activity,
            focus: self.focus,
            scroll: self.detail_scroll,
            width: self.detail_width,
            reading_scroll: self.conversation_scroll,
            following: self.follow_conversation,
            read_anchor: self.pending_anchor.clone(),
            read_excerpt: self.anchor_excerpt.clone(),
            audit_event: self.selected_event().map(str::to_owned),
            audit_top_event: self
                .activities
                .get(self.activity_top)
                .map(|activity| activity.event_id.clone()),
        });
        self.show_help = false;
        self.show_activity = false;
        self.detail_scroll = 0;
        self.detail_max = 0;
        self.detail_width = 0;
    }
    pub fn close_layer(&mut self) -> bool {
        let active = self.has_modal() || self.show_activity;
        if let Some(parent) = self.layers.pop() {
            self.picker = parent.picker;
            self.detail = parent.detail;
            self.show_help = parent.help;
            self.show_activity = parent.audit;
            self.focus = parent.focus;
            self.detail_scroll = parent.scroll;
            self.detail_width = parent.width;
            self.conversation_scroll = parent.reading_scroll;
            self.follow_conversation = parent.following;
            self.pending_anchor = parent.read_anchor;
            self.anchor_excerpt = parent.read_excerpt;
            if parent.audit {
                if let Some(id) = parent.audit_event {
                    if let Some(index) = self
                        .activities
                        .iter()
                        .position(|activity| activity.event_id == id)
                    {
                        self.selected = index;
                    } else {
                        self.notice = "原审计记录已移出最近400条预览；Ctrl+F查找持久原文".into();
                    }
                }
                if let Some(id) = parent.audit_top_event
                    && let Some(index) = self
                        .activities
                        .iter()
                        .position(|activity| activity.event_id == id)
                {
                    self.activity_top = index;
                }
            }
            self.cache_dirty = true;
        } else {
            self.picker = None;
            self.detail = None;
            self.show_help = false;
            self.show_activity = false;
            self.detail_scroll = 0;
        }
        active
    }
    pub fn open_detail(&mut self, title: impl Into<String>, text: impl Into<String>) {
        self.enter_layer();
        self.detail = Some((title.into(), Self::sanitize(&text.into())));
    }
    pub fn open_help(&mut self) {
        self.enter_layer();
        self.show_help = true;
    }
    pub fn draft_snapshot(&self) -> Draft {
        Draft {
            editor: self.editor.clone(),
            label: self.reply_label.clone(),
        }
    }
    pub fn empty_draft() -> Draft {
        Draft {
            editor: new_editor(),
            label: String::new(),
        }
    }
    pub fn restore_draft(&mut self, draft: Draft) {
        self.editor = draft.editor;
        self.reply_label = draft.label;
    }
    pub fn temporary_draft_text(&self) -> Option<String> {
        self.temporary_draft
            .as_ref()
            .map(|draft| draft.draft.editor.lines().join("\n"))
    }
    pub fn begin_temporary_draft(&mut self) {
        if self.temporary_draft.is_none() {
            let draft = self.draft_snapshot();
            self.enter_layer();
            let parent = self.layers.pop().unwrap();
            let layers = std::mem::take(&mut self.layers);
            self.temporary_draft = Some(TemporaryDraft {
                draft,
                layers,
                parent,
            });
            self.editor = new_editor();
            self.reply_label.clear();
            self.focus = Focus::Input;
        }
    }
    pub fn restore_temporary_draft(&mut self) -> bool {
        if let Some(saved) = self.temporary_draft.take() {
            self.restore_draft(saved.draft);
            self.layers = saved.layers;
            self.layers.push(saved.parent);
            self.close_layer();
            true
        } else {
            false
        }
    }
    pub fn open_picker(&mut self, kind: PickerKind, items: Vec<PickerItem>, query: String) {
        self.enter_layer();
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
        let mut picker = Picker {
            kind,
            items,
            query,
            selected,
            inline: true,
        };
        picker.selected = selected.min(filtered(&picker).len().saturating_sub(1));
        self.picker = Some(picker);
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
        self.retain_reading_anchor();
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
        if self.show_activity {
            match event.kind {
                MouseEventKind::ScrollUp => self.selected = self.selected.saturating_sub(3),
                MouseEventKind::ScrollDown => {
                    self.selected = (self.selected + 3).min(self.activities.len().saturating_sub(1))
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
        if self.show_activity {
            self.close_layer();
        } else {
            self.enter_layer();
            self.show_activity = true;
            self.focus = Focus::Activity;
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
        } else if self.show_activity {
            self.notice = "先按 Esc 或 F6 返回编辑，再粘贴到草稿".into();
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
        if key.code == KeyCode::F(1) {
            if self.show_help {
                self.close_layer();
            } else {
                self.open_help();
            }
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
                KeyCode::Esc => {
                    if self.is_completion() {
                        self.close_completion();
                    } else {
                        self.close_layer();
                    }
                }
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
        if key.code == KeyCode::Esc {
            self.close_layer();
            return;
        }
        if self.show_help || self.detail.is_some() {
            scroll_key(key.code, &mut self.detail_scroll, self.detail_max, 10);
            return;
        }
        match key.code {
            KeyCode::F(2) => {
                self.toggle_inspector();
                return;
            }
            KeyCode::F(6) => {
                if self.show_activity {
                    self.close_layer();
                }
                self.focus = if self.focus == Focus::Input {
                    Focus::Conversation
                } else {
                    Focus::Input
                };
                if self.focus == Focus::Conversation
                    && self.message_selected.is_none()
                    && !self.messages.is_empty()
                {
                    self.message_selected = Some(self.messages.len() - 1);
                    self.cache_dirty = true;
                }
                return;
            }
            KeyCode::Tab | KeyCode::BackTab if self.focus != Focus::Input => return,
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
                        self.focus = Focus::Conversation;
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
                    if self.follow_conversation {
                        self.unread = 0;
                    }
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
        _model: &str,
        status: &str,
    ) {
        let area = frame.area();
        frame.render_widget(
            Block::default().style(Style::default().bg(BACKGROUND).fg(TEXT)),
            area,
        );
        if area.width < 8 || area.height < 10 {
            frame.render_widget(
                Paragraph::new("请扩大终端窗口").style(Style::default().fg(ACCENT)),
                area,
            );
            return;
        }
        let input_width = area.width.saturating_sub(2).max(1) as usize;
        let input_height = wrap(&self.draft(), input_width).len().clamp(1, 5) as u16;
        let regions = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Min(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(input_height),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(area);
        let project = state
            .workspace
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        frame.render_widget(
            Paragraph::new(Line::styled(
                format!(" BONE / {}", Self::sanitize(&project)),
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            )),
            regions[0],
        );
        for index in [1, 3] {
            frame.render_widget(
                Paragraph::new("─".repeat(area.width as usize)).style(Style::default().fg(MUTED)),
                regions[index],
            );
        }
        if self.show_activity {
            self.render_activity(frame, regions[2]);
        } else {
            let inner = regions[2].inner(ratatui::layout::Margin {
                horizontal: 1,
                vertical: 0,
            });
            let width = inner.width.max(1) as usize;
            if self.cache_dirty || self.cache_width != width {
                self.retain_reading_anchor();
                let mut lines = Vec::new();
                let mut points = Vec::new();
                if self.messages.is_empty() {
                    lines.push(Line::styled(
                        "描述任务，BONE 会在当前工作区执行。",
                        Style::default().fg(ACCENT),
                    ));
                    lines.push(Line::styled(
                        "/ 命令   @ 文件   Ctrl+F 搜索   F1 帮助",
                        Style::default().fg(MUTED),
                    ));
                }
                self.message_offsets.clear();
                for (index, message) in self.messages.iter().enumerate() {
                    self.message_offsets.push(lines.len());
                    let (rows, offsets) = message_projection(
                        message,
                        width,
                        self.expanded.contains(&message_key(message)),
                        self.message_selected == Some(index),
                    );
                    let key = message_key(message);
                    let mut repeats = std::collections::HashMap::new();
                    points.extend(offsets.into_iter().map(|offset| {
                        let duplicate = repeats.entry(offset).or_insert(0);
                        let point = ReadPoint {
                            key: key.clone(),
                            offset,
                            duplicate: *duplicate,
                        };
                        *duplicate += 1;
                        point
                    }));
                    lines.extend(rows);
                }
                if let Some(mut anchor) = self.pending_anchor.take() {
                    let message = self
                        .messages
                        .iter()
                        .find(|message| message_key(message) == anchor.key);
                    let retained = message.is_some_and(|message| {
                        if self.anchor_excerpt.is_empty() {
                            return anchor.offset <= message.text.len();
                        }
                        if message
                            .text
                            .get(anchor.offset..)
                            .is_some_and(|suffix| suffix.starts_with(&self.anchor_excerpt))
                        {
                            return true;
                        }
                        if let Some(offset) = message.text.find(&self.anchor_excerpt) {
                            anchor.offset = offset;
                            return true;
                        }
                        false
                    });
                    if retained {
                        let exact = points.iter().position(|point| {
                            point.key == anchor.key
                                && point.offset == anchor.offset
                                && point.duplicate == anchor.duplicate
                        });
                        let preceding = points
                            .iter()
                            .enumerate()
                            .filter(|(_, point)| {
                                point.key == anchor.key && point.offset <= anchor.offset
                            })
                            .map(|(i, _)| i)
                            .next_back();
                        if let Some(index) = exact.or(preceding) {
                            self.conversation_scroll = index;
                        }
                    } else if let Some(index) =
                        points.iter().position(|point| point.key == anchor.key)
                    {
                        self.conversation_scroll = index;
                        self.notice =
                            "原观察未包含在最终结果中；保留同一调用，可查看原文/审计".into();
                    } else {
                        self.notice = "原阅读记录已移出预览；Ctrl+F 查找持久原文".into();
                    }
                    self.anchor_excerpt.clear();
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
                self.read_points = points;
                self.cache_width = width;
                self.cache_dirty = false;
            }
            self.conversation_max = self
                .transcript_cache
                .len()
                .saturating_sub(inner.height as usize);
            if self.follow_conversation {
                self.conversation_scroll = self.conversation_max;
                self.unread = 0;
            }
            self.conversation_scroll = self.conversation_scroll.min(self.conversation_max);
            frame.render_widget(
                Paragraph::new(
                    self.transcript_cache
                        .iter()
                        .skip(self.conversation_scroll)
                        .take(inner.height as usize)
                        .cloned()
                        .collect::<Vec<_>>(),
                ),
                inner,
            );
        }
        let fact = if self.live_status.is_empty() {
            status
        } else {
            &self.live_status
        };
        let fact = if self.notice.is_empty() {
            Self::sanitize(fact)
        } else {
            format!(
                "{} · {}",
                Self::sanitize(fact),
                Self::sanitize(&self.notice).replace('\n', " ")
            )
        };
        frame.render_widget(
            Paragraph::new(format!(" 当前：{fact}")).style(Style::default().fg(ACCENT)),
            regions[4],
        );
        let target = if self.reply_label.is_empty() {
            "新要求"
        } else {
            &self.reply_label
        };
        frame.render_widget(
            Paragraph::new(format!(" 输入目标：{}", Self::sanitize(target)))
                .style(Style::default().fg(MUTED)),
            regions[5],
        );
        self.editor
            .set_block(
                Block::default()
                    .borders(Borders::LEFT)
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
        frame.render_widget(&self.editor, regions[6]);
        let keys = if self.show_activity {
            "审计：↑↓ 选择 · Enter 原文 · Esc 返回"
        } else if self.focus == Focus::Input {
            "Enter 发送 · Shift+Enter 换行 · F6 阅读 · F2 审计 · F1 帮助"
        } else {
            "阅读：↑↓ 选择 · Enter 展开 · d 原文 · y 复制 · F6 输入 · F2 审计"
        };
        frame.render_widget(
            Paragraph::new(format!(" {keys}")).style(Style::default().fg(MUTED)),
            regions[7],
        );
        let position = if self.show_activity {
            format!(
                "审计 {}/{} · Esc 回到工作段落",
                self.selected + usize::from(!self.activities.is_empty()),
                self.activities.len()
            )
        } else if self.follow_conversation {
            "阅读：最新内容".into()
        } else {
            format!(
                "阅读：第 {} 行 · {} 次新内容更新 · End 到最新",
                self.conversation_scroll + 1,
                self.unread
            )
        };
        frame.render_widget(
            Paragraph::new(format!(" {position}")).style(Style::default().fg(if self.unread > 0 {
                ACCENT
            } else {
                MUTED
            })),
            regions[8],
        );
        if let Some(picker) = &self.picker {
            let rect = if picker.inline {
                let height = (filtered(picker).len().clamp(1, 6) + 2) as u16;
                Rect::new(
                    regions[6].x,
                    regions[6].y.saturating_sub(height),
                    regions[6].width,
                    height.min(regions[6].y.saturating_sub(area.y)),
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
            let help = "直接输入任务，Enter 发送。\nShift+Enter / Alt+Enter / Ctrl+J 换行；粘贴多行保持在输入框。\n↑↓ 移动光标；Alt+↑↓ 召回历史；Ctrl+←→ 移动单词。\nCtrl+A/E 行首尾；Ctrl+K 删除至行尾；Ctrl+W 删除单词。\nShift+方向键选区；Ctrl+Y复制选区或当前原文。\nCtrl+Z 撤销 / Alt+Z 重做；Ctrl+F 搜索对话。\nCtrl+P 命令菜单；Ctrl+O / @文件 Tab 引用；Ctrl+G 外部编辑器。\nF6 切换输入与阅读；Tab 只插入补全或编辑。Esc 返回上一层。\n对话：↑↓ / j k 选择消息；Enter 展开折叠；d 原文；y 复制。\nPageUp / PageDown 滚动，Home / End 到首尾。\nAgent 交付保留完整段落；工具默认摘要，Enter 展开。\n窗口最多256条/8MiB，单条128KiB预览。\n展开预览；d 原文 / y 复制完整记录；Ctrl+F 搜索持久历史。\n活动展示最近 400 条：上下选择，Enter 浏览原生事件详情。\nF2 打开审计；Esc 返回原阅读位置。\nCtrl+C 暂停；Ctrl+Y 复制选区或原文；Ctrl+R 恢复；Ctrl+Q 退出。\n\nF1 或 Esc 关闭帮助。";
            let commands = super::COMMANDS
                .iter()
                .map(|(name, _)| *name)
                .collect::<Vec<_>>()
                .join(" ");
            let help_text = format!("{help}\n\n{commands}");
            let width = overlay_rect(area).width.saturating_sub(2).max(1) as usize;
            preserve_overlay_offset(
                &help_text,
                self.detail_width,
                width,
                &mut self.detail_scroll,
            );
            self.detail_width = width;
            self.detail_max = render_overlay(
                frame,
                area,
                " 帮助 · ↑↓ / PageDown 滚动 · Esc 返回 ",
                &help_text,
                &mut self.detail_scroll,
            );
        } else if let Some((title, detail)) = &self.detail {
            let width = overlay_rect(area).width.saturating_sub(2).max(1) as usize;
            preserve_overlay_offset(detail, self.detail_width, width, &mut self.detail_scroll);
            self.detail_width = width;
            self.detail_max = render_overlay(
                frame,
                area,
                &format!(
                    " {} · Ctrl+P /audit 审计 · Esc 返回 ",
                    Self::sanitize(title)
                ),
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
                " 审计 · 只读 · 最近 400 · Enter 原文 · Esc 返回 ",
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
    let mut result = Vec::new();
    let fallback = if status == Color::Red {
        let mut lines = message.text.lines().filter(|line| !line.trim().is_empty());
        let first = lines.next().unwrap_or("失败");
        if first == "失败" || first == "错误" {
            format!("{first}：{}", lines.next().unwrap_or("查看原文获取原因"))
        } else {
            first.to_owned()
        }
    } else {
        message.text.lines().next().unwrap_or("").to_owned()
    };
    let summary = message.summary.as_deref().unwrap_or(&fallback);
    if tool && !expanded {
        result.push(Line::styled(
            fit_line(
                &format!(
                    "{} {} · {}",
                    if selected { "›" } else { " " },
                    if status == Color::Red {
                        summary
                    } else {
                        &message.role
                    },
                    if status == Color::Red {
                        &message.role
                    } else {
                        summary
                    }
                ),
                width,
            ),
            Style::default().fg(status),
        ));
        if status == Color::Blue
            && let Some((_, streams)) = message.text.split_once("stdout:\n")
        {
            let (stdout, stderr) = streams.split_once("\nstderr:\n").unwrap_or((streams, ""));
            for (label, stream) in [("stdout", stdout), ("stderr", stderr)] {
                if let Some(tail) = stream.lines().rfind(|line| !line.trim().is_empty()) {
                    result.push(Line::styled(
                        fit_line(&format!("{label}: {tail}"), width),
                        Style::default().fg(MUTED),
                    ));
                }
            }
        }
    } else {
        result.push(Line::styled(
            format!("{} {}", if selected { "›" } else { " " }, message.role),
            Style::default().fg(status).add_modifier(Modifier::BOLD),
        ));
        if tool {
            result.extend(
                wrap(&message.text, width)
                    .into_iter()
                    .map(|s| Line::styled(s, Style::default().fg(MUTED))),
            );
        } else {
            result.extend(markdown(&message.text, width));
        }
    }
    if !tool || expanded {
        result.push(Line::from(""));
    }
    result
}

fn fit_line(text: &str, width: usize) -> String {
    let mut result = String::new();
    let mut cells = 0;
    let full_width = UnicodeWidthStr::width(text);
    for grapheme in text.graphemes(true) {
        let size = UnicodeWidthStr::width(grapheme);
        if cells + size > width.saturating_sub(usize::from(full_width > width)) {
            break;
        }
        result.push_str(grapheme);
        cells += size;
    }
    if full_width > width && width > 0 {
        result.push('…');
    }
    result
}

// Map displayed rows back to source bytes, skipping Markdown syntax that is not rendered.
fn source_offsets(text: &str, rows: &[Line<'_>]) -> Vec<usize> {
    let mut cursor = 0;
    rows.iter()
        .map(|row| {
            let rendered = row
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>();
            let mut start = None;
            for grapheme in rendered.graphemes(true) {
                let box_drawing = grapheme
                    .chars()
                    .all(|ch| ('\u{2500}'..='\u{257f}').contains(&ch));
                // Table borders are generated decorations. A later literal border is another source object.
                if box_drawing && !text[cursor..].starts_with(grapheme) {
                    continue;
                }
                if let Some(relative) = text[cursor..].find(grapheme) {
                    cursor += relative;
                    start.get_or_insert(cursor);
                    cursor += grapheme.len();
                }
            }
            start.unwrap_or(cursor)
        })
        .collect()
}

fn message_projection(
    message: &Message,
    width: usize,
    expanded: bool,
    selected: bool,
) -> (Vec<Line<'static>>, Vec<usize>) {
    let lines = message_lines(message, width, expanded, selected);
    let mut offsets = vec![0; lines.len()];
    let compact_tool = message.role.starts_with("工具") && !expanded;
    let body_end = lines.len().saturating_sub(usize::from(!compact_tool));
    if body_end > 1 {
        let body = &lines[1..body_end];
        let mapped = source_offsets(&message.text, body);
        offsets[1..body_end].copy_from_slice(&mapped);
        if !compact_tool {
            // Blank paragraph separators retain the preceding source location.
            offsets[body_end] = offsets[body_end - 1];
        }
    }
    (lines, offsets)
}

fn preserve_overlay_offset(text: &str, old_width: usize, new_width: usize, scroll: &mut usize) {
    if old_width == 0 || old_width == new_width {
        return;
    }
    let rows = |width| {
        wrap(text, width)
            .into_iter()
            .map(Line::from)
            .collect::<Vec<_>>()
    };
    let old = source_offsets(text, &rows(old_width));
    let offset = old.get(*scroll).copied().unwrap_or(0);
    let new = source_offsets(text, &rows(new_width));
    *scroll = new
        .iter()
        .enumerate()
        .filter(|(_, byte)| **byte <= offset)
        .map(|(index, _)| index)
        .next_back()
        .unwrap_or(0);
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
    let inset_y = (area.height / 8).min(3);
    area.inner(ratatui::layout::Margin {
        // Full rows prevent background wide glyphs from crossing the modal's side borders.
        horizontal: 0,
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
