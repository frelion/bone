//! Terminal presentation of the Session interface. Execution remains owned by Engine.
mod services;
mod view;

use anyhow::{Context, Result, bail, ensure};
use bone::config::{ModelReference, Profile};
use bone::runtime::Engine;
use bone::state::{Event, JobState};
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event as TerminalEvent, EventStream, KeyCode, KeyEventKind, KeyModifiers,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use futures_util::StreamExt;
use ratatui::DefaultTerminal;
use serde_json::Value;
use services::native_text;
use std::collections::BTreeMap;
use std::io::{IsTerminal, stdout};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::time::Instant;
use view::{Activity, Draft, Focus, Message, PickerItem, PickerKind, Tone, View};

/// Concrete launch recipes, not a second runtime or model abstraction.
pub(super) struct Settings {
    pub profile_name: String,
    pub profile: Profile,
    pub profiles: Vec<(String, Profile)>,
}

pub(super) fn require_terminal() -> Result<()> {
    ensure!(
        std::io::stdin().is_terminal() && std::io::stdout().is_terminal(),
        "TUI requires an interactive terminal; use bone chat for piped input"
    );
    ensure!(
        std::env::var("TERM").as_deref() != Ok("dumb"),
        "TUI requires a terminal that supports cursor positioning"
    );
    Ok(())
}
struct Terminal {
    terminal: DefaultTerminal,
}
impl Terminal {
    fn open() -> Result<Self> {
        let terminal = ratatui::init();
        let guard = Self { terminal };
        guard.enable()?;
        Ok(guard)
    }
    fn enable(&self) -> Result<()> {
        execute!(
            stdout(),
            EnableBracketedPaste,
            EnableMouseCapture,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;
        Ok(())
    }
    fn suspend(&self) {
        let _ = execute!(
            stdout(),
            PopKeyboardEnhancementFlags,
            DisableMouseCapture,
            DisableBracketedPaste
        );
        ratatui::restore();
    }
    fn reopen(&mut self) -> Result<()> {
        // Reuse the backend and fullscreen viewport. Reconstructing Terminal
        // queries the cursor while an EventStream reader is being torn down.
        crossterm::terminal::enable_raw_mode()?;
        execute!(stdout(), crossterm::terminal::EnterAlternateScreen)?;
        self.enable()?;
        self.terminal.resize(self.terminal.size()?.into())?;
        Ok(())
    }
}
impl Drop for Terminal {
    fn drop(&mut self) {
        self.suspend();
    }
}

struct App {
    ui: View,
    cursor: Option<String>,
    older: Option<String>,
    settings: Settings,
    parts: BTreeMap<String, BTreeMap<u64, String>>,
    last_input: Option<String>,
    last_saved: services::UiSaved,
    changed_at: Option<Instant>,
    elapsed_from: Instant,
    file_range: Option<std::ops::Range<usize>>,
    file_index: Option<tokio::task::JoinHandle<Result<Vec<String>>>>,
    files: Option<Vec<String>>,
    session_index: Option<tokio::task::JoinHandle<Result<Vec<PickerItem>>>>,
    diff: Option<tokio::task::JoinHandle<Result<String>>>,
    export: Option<tokio::task::JoinHandle<Result<PathBuf>>>,
    search: Option<tokio::task::JoinHandle<Result<Vec<PickerItem>>>>,
    search_query: String,
    completion_key: Option<(String, usize)>,
    tool_previews: BTreeMap<String, Message>,
    tool_observations: BTreeMap<String, (String, String)>,
    reply_target: Option<String>,
    drafts: BTreeMap<String, Draft>,
    reconcile_call: Option<String>,
    detail_event: Option<String>,
    detail_sources: BTreeMap<String, String>,
    last_tool_failure: Option<String>,
    notice_seen: String,
    notice_until: Option<Instant>,
    mouse_capture: bool,
}
impl App {
    fn new(settings: Settings) -> Self {
        Self {
            ui: View::new(),
            cursor: None,
            older: None,
            settings,
            parts: BTreeMap::new(),
            last_input: None,
            last_saved: services::UiSaved::default(),
            changed_at: None,
            elapsed_from: Instant::now(),
            file_range: None,
            file_index: None,
            files: None,
            session_index: None,
            diff: None,
            export: None,
            search: None,
            search_query: String::new(),
            completion_key: None,
            tool_previews: BTreeMap::new(),
            tool_observations: BTreeMap::new(),
            reply_target: None,
            drafts: BTreeMap::new(),
            reconcile_call: None,
            detail_event: None,
            detail_sources: BTreeMap::new(),
            last_tool_failure: None,
            notice_seen: String::new(),
            notice_until: None,
            mouse_capture: true,
        }
    }
    fn load(&mut self, engine: &mut Engine, data: &Path) -> Result<()> {
        self.ui = View::new();
        self.parts.clear();
        self.tool_previews.clear();
        self.tool_observations.clear();
        self.reply_target = None;
        self.drafts.clear();
        self.reconcile_call = None;
        self.detail_event = None;
        self.detail_sources.clear();
        self.last_tool_failure = None;
        self.notice_seen.clear();
        self.notice_until = None;
        self.completion_key = None;
        self.search_query.clear();
        abort_task(&mut self.search);
        self.last_input = None;
        self.cursor = None;
        self.older = None;
        self.files = None;
        self.file_range = None;
        abort_task(&mut self.file_index);
        abort_task(&mut self.session_index);
        abort_task(&mut self.diff);
        abort_task(&mut self.export);
        // Read one original at a time: native responses can be several MiB.
        // Keep IDs during the backward traversal, then project in append order.
        let mut ids = Vec::new();
        let mut before = None;
        let mut more = false;
        for _ in 0..40 {
            let page = bone::history_before(data, &engine.state().id, before.as_deref(), 1)?;
            more = page.has_more;
            let Some(event) = page.events.first() else {
                break;
            };
            ids.push(event.id.clone());
            before = Some(event.id.clone());
            if !more {
                break;
            }
        }
        self.older = ids.last().cloned();
        self.cursor = ids.first().cloned();
        for id in ids.iter().rev() {
            self.ingest(engine, &engine.read_event(id)?)?;
        }
        for question in engine.unanswered_questions() {
            if !ids.contains(&question.id) {
                self.ingest(engine, &engine.read_event(&question.id)?)?;
            }
        }
        self.ui.notice = if more {
            "已加载最近记录；/older 往前翻页 · Ctrl+F 搜索全部原文"
        } else {
            "输入 / 或 @ 显示候选 · Tab 补全 · Enter 发送 · F1 帮助"
        }
        .into();
        match services::load(data, &engine.state().id) {
            Ok(saved) => {
                if !saved.history.is_empty() {
                    self.ui.set_history(saved.history.clone());
                }
                self.ui.paste(&saved.draft);
                self.reply_target = saved.reply_to.clone();
                self.last_saved = saved;
                if !self.ui.draft().is_empty() {
                    self.ui.notice = "已恢复未发送的草稿；Enter 才发送".into();
                }
            }
            Err(error) => {
                self.last_saved = services::UiSaved::default();
                self.ui.notice = format!("草稿恢复失败：{error:#}");
            }
        }
        self.changed_at = None;
        self.elapsed_from = Instant::now();
        engine.drain_model_progress();
        engine.drain_tool_progress();
        self.refresh_usage(engine);
        self.metadata(engine);
        Ok(())
    }
    fn refresh_latest(&mut self, engine: &mut Engine, data: &Path) -> Result<()> {
        ensure!(
            self.reconcile_call.is_none(),
            "先记录或取消核查表单，再返回最新记录；原草稿与核查草稿均保留"
        );
        self.save(engine, data)?;
        let active = self.ui.draft_snapshot();
        let inactive = std::mem::take(&mut self.drafts);
        let target = self.reply_target.clone();
        let refreshed = self.load(engine, data);
        self.ui.restore_draft(active);
        self.drafts = inactive;
        self.reply_target = target;
        self.metadata(engine);
        refreshed?;
        self.ui.notice = "已返回最新记录；草稿、光标、选区与回复目标已保留".into();
        Ok(())
    }
    fn save(&mut self, engine: &Engine, data: &Path) -> Result<()> {
        let saved = services::UiSaved {
            draft: self
                .ui
                .temporary_draft_text()
                .unwrap_or_else(|| self.ui.draft()),
            history: self.ui.history(),
            reply_to: self.reply_target.clone(),
        };
        if saved.draft != self.last_saved.draft
            || saved.history != self.last_saved.history
            || saved.reply_to != self.last_saved.reply_to
        {
            services::save(data, &engine.state().id, &saved)?;
            self.last_saved = saved;
        }
        self.changed_at = None;
        Ok(())
    }
    fn clear_preview(&mut self) {
        let keys: Vec<_> = self.parts.keys().cloned().collect();
        for call in keys {
            self.ui.remove_message(&format!("live:{call}"));
        }
        self.parts.clear();
        self.tool_observations.clear();
        for (_, original) in std::mem::take(&mut self.tool_previews) {
            self.ui.upsert_message(original);
        }
    }
    fn ingest(&mut self, engine: &Engine, event: &Event) -> Result<()> {
        let previous_head = self.ui.first_message_id().map(str::to_owned);
        if event.kind == "input_handled"
            && event.data["input_id"].as_str() == self.last_input.as_deref()
        {
            self.ui.notice = format!(
                "输入 {} 已纳入执行",
                short_id(self.last_input.as_deref().unwrap_or(""))
            );
        }
        if event.kind == "input" && event.data["source"] == "user" {
            self.last_input = Some(event.id.clone());
            self.ui
                .remember_prompt(&native_text(&event.data["message"]));
        }
        if self.last_input.is_none() {
            self.last_input = event.root_input.clone();
        }
        if matches!(
            event.kind.as_str(),
            "model_message" | "model_failed" | "cancelled"
        ) && let Some(call) = &event.call_id
        {
            self.ui.remove_message(&format!("live:{call}"));
            self.parts.remove(call);
        }
        if matches!(
            event.kind.as_str(),
            "tool_started" | "tool_result" | "tool_reconciled"
        ) && let Some(call) = &event.call_id
        {
            self.tool_previews.remove(call);
        }
        if event.kind == "question" && engine.is_unanswered_question(event) {
            self.ui.notice = format!(
                "问题 {} 待回答；/questions 显式选择回复目标",
                short_id(&event.id)
            );
        }
        if event.kind == "tool_result" && tool_failed(engine, event)? {
            self.last_tool_failure = Some(format!(
                "{} {}",
                event.data["tool_name"].as_str().unwrap_or("tool"),
                short_id(event.call_id.as_deref().unwrap_or(&event.id))
            ));
        }
        if event.reply_to.as_deref() == self.last_input.as_deref()
            && matches!(
                event.kind.as_str(),
                "delivery" | "failure" | "input_paused" | "input_resolved"
            )
        {
            self.ui.notice.clear();
        }
        ingest(engine, event, &mut self.ui)?;
        if event.kind == "tool_result"
            && let Some(call) = &event.call_id
            && let Some((stdout, stderr)) = self.tool_observations.remove(call)
        {
            let final_body = event_body(engine, event)?;
            if [&stdout, &stderr]
                .iter()
                .any(|text| !text.is_empty() && !final_body.contains(text.as_str()))
            {
                let key = format!("tool:{call}");
                if let Some(mut message) = self
                    .ui
                    .messages
                    .iter()
                    .find(|m| m.event_id.as_deref() == Some(&key))
                    .cloned()
                {
                    message.text.push_str("\n\n实时观察与最终持久正文不完全一致；此处展示最终记录，实时观察不作为交付证据。");
                    if let Some(summary) = &mut message.summary {
                        summary.push_str(" · 实时观察与最终原文不同");
                    }
                    self.ui.upsert_message(message);
                }
            }
        }
        self.update_older_after_trim(engine, previous_head.as_deref());
        Ok(())
    }
    fn update_older_after_trim(&mut self, engine: &Engine, previous: Option<&str>) {
        let current = self.ui.first_message_id();
        if previous.is_some() && current != previous {
            self.older = current
                .and_then(|id| {
                    if let Some(call) = id.strip_prefix("tool:") {
                        engine
                            .read_call_event(call, "tool_started")
                            .ok()
                            .map(|e| e.id)
                    } else if id.starts_with("live:") {
                        None
                    } else {
                        Some(id.to_owned())
                    }
                })
                .or_else(|| self.older.clone());
        }
    }
    fn sync(&mut self, engine: &Engine, data: &Path) -> Result<()> {
        loop {
            let page = bone::history_page(data, &engine.state().id, self.cursor.as_deref(), 20)?;
            if self.older.is_none() {
                self.older = page.events.first().map(|e| e.id.clone());
            }
            for event in &page.events {
                self.ingest(engine, event)?;
            }
            if let Some(next) = page.next_cursor {
                self.cursor = Some(next);
            }
            if !page.has_more {
                break;
            }
        }
        self.refresh_usage(engine);
        self.metadata(engine);
        Ok(())
    }
    fn metadata(&mut self, engine: &Engine) {
        let questions = engine.unanswered_questions();
        self.ui.reply_label = if let Some(call) = &self.reconcile_call {
            format!(
                "核查 {} · Enter 记录 · Esc 取消 · Ctrl+D 证据",
                short_id(call)
            )
        } else if let Some(id) = &self.reply_target {
            if let Some(question) = questions.iter().find(|q| &q.id == id) {
                format!(
                    "回复 {} · {} · Enter 发送 · /message",
                    short_id(id),
                    question_summary(question.data["question"].as_str().unwrap_or("问题"), 24)
                )
            } else {
                "回复目标失效 · 草稿保留 · /message 明确改发".into()
            }
        } else {
            format!(
                "新要求 · Enter 发送{}",
                if questions.is_empty() {
                    ""
                } else {
                    " · /questions 选择待答问题"
                }
            )
        };
        if self.ui.notice != self.notice_seen {
            self.notice_seen = self.ui.notice.clone();
            self.notice_until =
                (!self.ui.notice.is_empty()).then(|| Instant::now() + Duration::from_secs(8));
        }
        self.ui.busy = !engine.is_quiescent();
        self.ui.session_label = engine.state().id.chars().take(8).collect();
        self.ui.live_status = if self.ui.busy {
            format!(
                "{} · {}s",
                self.current_status(engine),
                self.elapsed_from.elapsed().as_secs()
            )
        } else {
            self.current_status(engine)
        };
    }
    fn refresh_usage(&mut self, engine: &Engine) {
        self.ui.usage = if let Some(input) = &self.last_input {
            let root = engine
                .read_event(input)
                .ok()
                .and_then(|e| e.root_input)
                .unwrap_or_else(|| input.clone());
            let m = engine.metrics(&root);
            let tokens = m["total_tokens"]
                .as_u64()
                .map(|n| n.to_string())
                .unwrap_or_else(|| "?".into());
            format!(
                "本次 {} / {} calls · {} tokens{}",
                m["model_calls"],
                engine.options().max_calls,
                tokens,
                if engine.options().read_only {
                    " · 只读"
                } else {
                    " · 可写"
                }
            )
        } else {
            if engine.options().read_only {
                "只读"
            } else {
                "可写"
            }
            .into()
        };
    }
    fn progress(&mut self, engine: &mut Engine) -> bool {
        let previous_head = self.ui.first_message_id().map(str::to_owned);
        let mut changed = false;
        for progress in engine.drain_model_progress() {
            if progress.purpose == "summary"
                || engine.state().focus.as_ref() != Some(&progress.job_id)
                || engine
                    .state()
                    .jobs
                    .get(&progress.job_id)
                    .and_then(|j| j.current_call.as_ref())
                    != Some(&progress.call_id)
            {
                continue;
            }
            let item = &progress.item;
            if item["item"] != "event" {
                continue;
            }
            let value = &item["value"];
            let part = value["part"].as_u64().unwrap_or(0);
            let text = match value["event"].as_str() {
                Some("text") => value["text"].as_str(),
                Some("end") if value["content"]["type"] == "text" => {
                    value["content"]["text"].as_str()
                }
                _ => None,
            };
            let Some(text) = text else {
                continue;
            };
            let parts = self.parts.entry(progress.call_id.clone()).or_default();
            if parts.len() >= 32 && !parts.contains_key(&part) {
                continue;
            }
            let buffer = parts.entry(part).or_default();
            if value["event"] == "end" {
                buffer.clear();
            }
            let remaining = 32_768usize.saturating_sub(buffer.len());
            let clipped: String = text
                .chars()
                .scan(0usize, |bytes, c| {
                    *bytes += c.len_utf8();
                    (*bytes <= remaining).then_some(c)
                })
                .collect();
            buffer.push_str(&clipped);
            let text = parts.values().cloned().collect::<Vec<_>>().join("\n\n");
            self.ui.upsert_message(Message {
                summary: None,
                role: "Agent · 输出中（未交付）".into(),
                text,
                event_id: Some(format!("live:{}", progress.call_id)),
            });
            changed = true;
        }
        for progress in engine.drain_tool_progress() {
            let key = format!("tool:{}", progress.call_id);
            if progress.revision != engine.state().revision {
                continue;
            }
            if !self.tool_previews.contains_key(&progress.call_id)
                && let Some(original) = self
                    .ui
                    .messages
                    .iter()
                    .find(|m| m.event_id.as_deref() == Some(&key))
                    .cloned()
            {
                self.tool_previews
                    .insert(progress.call_id.clone(), original);
            }
            self.tool_observations.insert(
                progress.call_id.clone(),
                (progress.stdout.clone(), progress.stderr.clone()),
            );
            self.ui.upsert_message(Message {
                summary: None,
                role: format!("工具 · {}", progress.tool_name),
                text: format!(
                    "执行中 · 实时日志（尾部预览，最终记录保留原文）\nstdout:\n{}\nstderr:\n{}",
                    progress.stdout, progress.stderr
                ),
                event_id: Some(key),
            });
            changed = true;
        }
        self.update_older_after_trim(engine, previous_head.as_deref());
        changed
    }
    fn commands(&mut self, query: String) {
        self.ui
            .open_picker(PickerKind::Command, command_items(), query);
    }
    fn sessions(&mut self, engine: &Engine, data: &Path) -> Result<()> {
        self.ui
            .open_picker(PickerKind::Session, Vec::new(), String::new());
        self.ui.notice = "正在读取项目会话…".into();
        abort_task(&mut self.session_index);
        let data = data.to_owned();
        let workspace = engine.state().workspace.clone();
        let current = engine.state().id.clone();
        self.session_index = Some(tokio::task::spawn_blocking(move || {
            session_items(&data, &workspace, &current)
        }));
        Ok(())
    }
    fn models(&mut self) {
        let current = model_label(&self.settings.profile);
        let mut matched = false;
        let mut items = Vec::new();
        for (name, profile) in &self.settings.profiles {
            let label = model_label(profile);
            let active = *name == self.settings.profile_name && label == current;
            matched |= active;
            items.push(PickerItem {
                label: format!("{label}{}", if active { " · 当前" } else { "" }),
                detail: format!("配置：{name}"),
                value: name.clone(),
            });
        }
        if !matched {
            items.insert(
                0,
                PickerItem {
                    label: format!("{current} · 当前"),
                    detail: "当前配置".into(),
                    value: "@current".into(),
                },
            );
        }
        self.ui.open_picker(PickerKind::Model, items, String::new());
    }
    fn complete_file(&mut self, engine: &Engine) {
        let cursor = self.ui.cursor();
        let prefix = &self.ui.draft()[..cursor];
        let start = prefix
            .rfind('@')
            .filter(|&n| n == 0 || prefix[..n].ends_with(char::is_whitespace));
        self.file_range = Some(start.unwrap_or(cursor)..cursor);
        let query = start
            .map(|n| prefix[n + 1..].trim_matches('"').to_owned())
            .unwrap_or_default();
        if let Some(files) = &self.files {
            self.ui
                .open_picker(PickerKind::File, file_items(files), query);
        } else {
            self.ui.notice = "正在索引项目文件…".into();
            self.ui.open_picker(PickerKind::File, Vec::new(), query);
            let workspace = engine.state().workspace.clone();
            if self.file_index.is_none() {
                self.file_index = Some(tokio::task::spawn_blocking(move || {
                    services::files(&workspace)
                }));
            }
        }
    }
    fn switch_session(&mut self, engine: &mut Engine, data: &Path, id: Option<&str>) -> Result<()> {
        if id == Some(engine.state().id.as_str()) {
            self.ui.picker = None;
            return Ok(());
        }
        self.save(engine, data)?;
        pause(engine, &mut self.ui)?;
        self.clear_preview();
        let next = Engine::open(
            data,
            &engine.state().workspace,
            id,
            self.settings.profile.clone(),
            self.settings.profile_name.clone(),
            engine.options().clone(),
        )?;
        *engine = next;
        self.drafts.clear();
        self.load(engine, data)?;
        self.ui.notice = if id.is_some() {
            "会话已打开；未完成工作保持暂停，Ctrl+R 继续"
        } else {
            "新会话已创建；直接描述任务"
        }
        .into();
        Ok(())
    }
    fn set_model(&mut self, engine: &mut Engine, value: &str) -> Result<()> {
        if value == "@current" {
            self.ui.picker = None;
            return Ok(());
        }
        let (name, profile) = if let Some((name, profile)) = self
            .settings
            .profiles
            .iter()
            .find(|(name, _)| name == value)
        {
            (name.clone(), profile.clone())
        } else if value == self.settings.profile_name {
            (value.to_owned(), self.settings.profile.clone())
        } else {
            let profile = profile_with_model(&self.settings.profile, value)?;
            (self.settings.profile_name.clone(), profile)
        };
        profile.validate()?;
        pause(engine, &mut self.ui)?;
        engine.set_profile(profile.clone(), name.clone())?;
        self.settings.profile_name = name;
        self.settings.profile = profile;
        self.clear_preview();
        self.ui.picker = None;
        self.ui.notice = "模型已切换；工作保留，Ctrl+R 继续。配置文件未修改".into();
        Ok(())
    }
    fn refresh_completion(&mut self, engine: &Engine, data: &Path) -> Result<()> {
        let draft = self.ui.draft();
        let cursor = self.ui.cursor();
        let key = (draft.clone(), cursor);
        if self.completion_key.as_ref() == Some(&key) {
            return Ok(());
        }
        self.completion_key = Some(key);
        if self.reconcile_call.is_some() || self.ui.focus != Focus::Input || self.ui.has_modal() {
            return Ok(());
        }
        let prefix = &draft[..cursor];
        if is_complete_command(prefix) && cursor == draft.len() {
            self.ui.close_completion();
        } else if draft.starts_with('/') && !prefix.contains(char::is_whitespace) {
            self.ui
                .open_completion(PickerKind::Command, command_items(), prefix.to_owned());
        } else if let Some(query) = prefix.strip_prefix("/model ") {
            self.models();
            let items = self.ui.picker.take().map(|p| p.items).unwrap_or_default();
            self.ui
                .open_completion(PickerKind::Model, items, query.to_owned());
        } else if let Some(query) = prefix.strip_prefix("/sessions ") {
            self.sessions(engine, data)?;
            self.ui
                .open_completion(PickerKind::Session, Vec::new(), query.to_owned());
        } else if prefix.rfind('@').is_some_and(|n| {
            (n == 0 || prefix[..n].ends_with(char::is_whitespace))
                && (!prefix[n + 1..].contains(char::is_whitespace)
                    || (prefix[n + 1..].starts_with('"')
                        && prefix[n + 1..].matches('"').count() == 1))
        }) {
            self.complete_file(engine);
            let picker = self
                .ui
                .picker
                .take()
                .context("file completion was not opened")?;
            self.ui
                .open_completion(PickerKind::File, picker.items, picker.query);
        } else {
            self.ui.close_completion();
        }
        Ok(())
    }
    fn complete_inline(&mut self, kind: PickerKind, value: &str) {
        self.ui.close_completion();
        match kind {
            PickerKind::Command => {
                self.ui.take_draft();
                self.ui.paste(&format!("{value} "));
            }
            PickerKind::Model => {
                self.ui.take_draft();
                self.ui.paste(&format!("/model {value}"));
            }
            PickerKind::Session => {
                self.ui.take_draft();
                self.ui.paste(&format!("/sessions {value}"));
            }
            PickerKind::File => {
                let reference = if value.contains(char::is_whitespace) {
                    format!("@\"{value}\" ")
                } else {
                    format!("@{value} ")
                };
                if let Some(range) = self.file_range.take() {
                    self.ui.replace_range(range, &reference);
                } else {
                    self.ui.paste(&reference);
                }
            }
            _ => {}
        }
        self.completion_key = Some((self.ui.draft(), self.ui.cursor()));
        self.ui.notice = "已补全；Enter 才执行或发送".into();
    }
    fn start_search(&mut self, engine: &Engine, data: &Path, query: &str) {
        abort_task(&mut self.search);
        self.ui
            .open_picker(PickerKind::History, Vec::new(), query.to_owned());
        self.search_query.clear();
        self.refresh_search(engine, data);
    }
    fn refresh_search(&mut self, engine: &Engine, data: &Path) {
        let Some(query) = self
            .ui
            .picker
            .as_ref()
            .filter(|p| p.kind == PickerKind::History)
            .map(|p| p.query.clone())
        else {
            return;
        };
        if query == self.search_query {
            return;
        }
        self.search_query = query.clone();
        abort_task(&mut self.search);
        if let Some(picker) = &mut self.ui.picker {
            picker.items.clear();
        }
        if query.trim().is_empty() {
            return;
        }
        let data = data.to_owned();
        let id = engine.state().id.clone();
        self.ui.notice = "正在搜索完整会话原文…".into();
        self.search = Some(tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            tokio::task::spawn_blocking(move || {
                Ok(bone::history_search(&data, &id, &query, 100)?
                    .into_iter()
                    .map(|m| PickerItem {
                        label: m.snippet,
                        detail: format!("{} · {}", m.kind, m.event_id),
                        value: m.event_id,
                    })
                    .collect())
            })
            .await?
        }));
    }
    fn questions(&mut self, engine: &Engine) {
        let items = engine
            .unanswered_questions()
            .iter()
            .map(|q| PickerItem {
                label: q.data["question"].as_str().unwrap_or("问题").into(),
                detail: format!(
                    "{}{}",
                    q.id,
                    if self.reply_target.as_ref() == Some(&q.id) {
                        " · 当前回复目标"
                    } else {
                        ""
                    }
                ),
                value: q.id.clone(),
            })
            .collect();
        self.ui
            .open_picker(PickerKind::Question, items, String::new());
        self.ui.notice = "选择要回答的问题；Esc 返回，草稿保留".into();
    }
    fn switch_target(&mut self, target: Option<String>) {
        while self.ui.has_modal() || self.ui.show_activity {
            self.ui.close_layer();
        }
        self.ui.focus = Focus::Input;
        if self.reply_target == target {
            return;
        }
        let previous = self.reply_target.clone().unwrap_or_default();
        self.drafts.insert(previous, self.ui.draft_snapshot());
        let next = target.clone().unwrap_or_default();
        let draft = self.drafts.remove(&next).unwrap_or_else(View::empty_draft);
        self.ui.restore_draft(draft);
        self.reply_target = target;
        self.ui.focus = Focus::Input;
    }
    fn bind_reply(&mut self, engine: &Engine, id: &str) -> Result<()> {
        ensure!(
            engine.unanswered_questions().iter().any(|q| q.id == id),
            "回复目标已失效；草稿和原目标保留，/questions 查看待答问题"
        );
        self.switch_target(Some(id.to_owned()));
        self.ui.notice = format!(
            "已选择回复 {}；Enter 回答，/message 返回新要求草稿",
            short_id(id)
        );
        Ok(())
    }
    fn cancel_reply(&mut self) {
        self.switch_target(None);
        self.ui.notice = "已切换新要求草稿；Enter 发送新要求".into();
    }
    fn begin_reconcile(&mut self, engine: &mut Engine, call: &str) -> Result<()> {
        ensure!(
            self.reconcile_call.is_none(),
            "已有核查表单；Esc 取消后重新选择"
        );
        ensure!(
            engine.state().unknown_writes.contains_key(call),
            "该未知写入已失效；原草稿保留"
        );
        pause(engine, &mut self.ui)?;
        self.ui.begin_temporary_draft();
        self.reconcile_call = Some(call.to_owned());
        self.ui.notice =
            "核查表单：填写检查过程和实际效果；Enter 记录，Esc 取消，Ctrl+D 阅读证据".into();
        self.metadata(engine);
        Ok(())
    }
    fn reconcile_editor_active(&self) -> bool {
        self.reconcile_call.is_some()
            && self.ui.focus == Focus::Input
            && !self.ui.has_modal()
            && !self.ui.show_activity
    }
    fn handle_reconcile_key(
        &mut self,
        engine: &mut Engine,
        key: crossterm::event::KeyEvent,
    ) -> Result<bool> {
        if !self.reconcile_editor_active() {
            return Ok(false);
        }
        match key.code {
            KeyCode::Esc => self.cancel_reconcile(),
            KeyCode::Enter
                if !key.modifiers.intersects(
                    KeyModifiers::ALT | KeyModifiers::CONTROL | KeyModifiers::SHIFT,
                ) =>
            {
                self.submit_reconcile(engine)?
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
    fn cancel_reconcile(&mut self) {
        self.ui.restore_temporary_draft();
        self.reconcile_call = None;
        self.ui.notice = "已取消核查；原草稿、目标与阅读位置已恢复，执行仍暂停".into();
    }
    fn submit_reconcile(&mut self, engine: &mut Engine) -> Result<()> {
        let call = self.reconcile_call.as_deref().context("没有核查表单")?;
        engine.resolve_write(call, self.ui.draft().trim())?;
        self.ui.restore_temporary_draft();
        self.reconcile_call = None;
        self.ui.notice = "已记录核查结果；执行仍暂停，Ctrl+R 显式恢复".into();
        Ok(())
    }
    fn expire_notice(&mut self) -> bool {
        if self
            .notice_until
            .is_some_and(|until| Instant::now() >= until)
        {
            self.ui.notice.clear();
            self.notice_seen.clear();
            self.notice_until = None;
            return true;
        }
        false
    }
    fn current_status(&self, engine: &Engine) -> String {
        current_status(
            engine,
            self.last_input.as_deref(),
            self.last_tool_failure.as_deref(),
        )
    }
    fn open_delivery(&mut self, engine: &Engine, data: &Path) -> Result<()> {
        let event =
            services::latest_delivery(data, &engine.state().id)?.context("此会话还没有持久交付")?;
        self.detail_event = Some(event.id.clone());
        self.ui.open_detail(
            format!(
                "交付 {} · 输入 {}",
                short_id(&event.id),
                short_id(event.reply_to.as_deref().unwrap_or(""))
            ),
            detail(engine, &event)?,
        );
        self.remember_detail_source();
        Ok(())
    }
    fn remember_detail_source(&mut self) {
        if let (Some((title, _)), Some(id)) = (&self.ui.detail, &self.detail_event) {
            self.detail_sources.insert(title.clone(), id.clone());
        }
    }
    fn open_audit(&mut self, engine: &Engine) -> Result<()> {
        let id = if let Some((title, _)) = &self.ui.detail {
            self.detail_sources.get(title).cloned()
        } else {
            self.selected_record(engine)
        }
        .context("先选择持久记录，再查看审计元信息")?;
        self.detail_event = Some(id.clone());
        self.ui.open_detail(
            format!("审计原文 · {}", short_id(&id)),
            audit_detail(&engine.read_event(&id)?)?,
        );
        self.remember_detail_source();
        Ok(())
    }
    fn reconciliations(&mut self, engine: &Engine) {
        let items = engine
            .state()
            .unknown_writes
            .values()
            .map(|w| PickerItem {
                label: format!("{} · 结果未知", w.tool_name),
                detail: w.call_id.clone(),
                value: w.call_id.clone(),
            })
            .collect();
        self.ui
            .open_picker(PickerKind::Reconcile, items, String::new());
        self.ui.notice = "先检查实际文件/进程结果，记录核查结论后显式恢复".into();
    }
    fn selected_record(&self, engine: &Engine) -> Option<String> {
        let id = self.ui.selected_message_id()?;
        if let Some(call) = id.strip_prefix("tool:") {
            engine
                .read_call_event(call, "tool_result")
                .or_else(|_| engine.read_call_event(call, "tool_started"))
                .ok()
                .map(|e| e.id)
        } else if id.starts_with("live:") {
            None
        } else {
            Some(id.to_owned())
        }
    }
    fn open_selected(&mut self, engine: &Engine) -> Result<()> {
        let id = self
            .selected_record(engine)
            .context("选择一条持久消息后查看原文")?;
        let observation = self
            .ui
            .selected_message()
            .filter(|m| m.text.starts_with("执行中 · 实时日志"))
            .map(|m| m.text.clone());
        let source = detail(engine, &engine.read_event(&id)?)?;
        let title = if observation.is_some() {
            format!("实时观察快照 · {} · 未交付", short_id(&id))
        } else {
            format!("结果原文 · {} · D 审计", short_id(&id))
        };
        let text = observation
            .map(|text| format!("{text}\n\n已持久化启动记录\n{source}"))
            .unwrap_or(source);
        self.detail_event = Some(id);
        self.ui.open_detail(title, text);
        self.remember_detail_source();
        Ok(())
    }
    async fn copy_selected(&mut self, engine: &Engine) -> Result<()> {
        ensure!(
            !self.ui.show_help,
            "当前对象是键盘帮助；Esc 返回后选择草稿或持久记录复制"
        );
        let (object, text) = if let Some((title, text)) = &self.ui.detail {
            (format!("详情 {title}"), text.clone())
        } else if self.ui.picker.is_some() {
            let (_, value) = self
                .ui
                .picker_value()
                .context("当前候选列表没有选中的对象")?;
            ("当前候选".into(), value)
        } else if self.ui.focus == Focus::Input {
            if let Some(selection) = self.ui.selected_input_text() {
                ("输入选区".into(), selection)
            } else {
                let label = if self.reconcile_call.is_some() {
                    "核查草稿"
                } else if self.reply_target.is_some() {
                    "回复草稿"
                } else {
                    "新要求草稿"
                };
                (label.into(), self.ui.draft())
            }
        } else if self.ui.focus == Focus::Conversation
            && self
                .ui
                .selected_message()
                .is_some_and(|m| m.text.starts_with("执行中 · 实时日志"))
        {
            (
                "实时观察尾部（未交付）".into(),
                self.ui.selected_message().unwrap().text.clone(),
            )
        } else {
            let id = if self.ui.focus == Focus::Activity {
                self.ui.selected_event().map(str::to_owned)
            } else {
                self.selected_record(engine)
            }
            .context("当前对象没有持久原文；选择一条持久消息后复制")?;
            let event = engine.read_event(&id)?;
            let body = if event.kind == "tool_started" {
                detail(engine, &event)?
            } else {
                event_body(engine, &event)?
            };
            (format!("{} {}", event.kind, short_id(&id)), body)
        };
        ensure!(!text.is_empty(), "{object}没有可复制文字");
        copy_to_clipboard(&text).await?;
        self.ui.notice = format!("已复制{object}完整文字");
        Ok(())
    }
    fn load_older(&mut self, engine: &Engine, data: &Path) -> Result<()> {
        let page = bone::history_before(data, &engine.state().id, self.older.as_deref(), 20)?;
        if page.events.is_empty() {
            self.ui.notice = "已经是最早的记录".into();
            return Ok(());
        }
        let mut earlier = View::new();
        for event in &page.events {
            ingest(engine, event, &mut earlier)?;
        }
        self.older = page.next_cursor;
        self.ui.prepend_messages(earlier.messages);
        self.ui.notice = if page.has_more {
            "已载入更早对话；/older 继续向前，Ctrl+F 搜索全记录"
        } else {
            "已载入最早对话"
        }
        .into();
        Ok(())
    }
    fn has_tasks(&self) -> bool {
        self.file_index.is_some()
            || self.session_index.is_some()
            || self.diff.is_some()
            || self.export.is_some()
            || self.search.is_some()
    }
    async fn tasks(&mut self) {
        if let Some(result) = finished_task(&mut self.search).await {
            match result {
                Ok(items) => {
                    if let Some(picker) =
                        self.ui.picker.as_mut().filter(|p| {
                            p.kind == PickerKind::History && p.query == self.search_query
                        })
                    {
                        picker.items = items;
                        picker.selected = 0;
                        self.ui.notice = "搜索完整持久记录；Enter 阅读原文，Esc 返回".into();
                    }
                }
                Err(error) => self.ui.notice = format!("搜索失败：{error:#}"),
            }
        }
        if let Some(result) = finished_task(&mut self.session_index).await {
            match result {
                Ok(items) => {
                    if let Some(picker) = self
                        .ui
                        .picker
                        .as_mut()
                        .filter(|p| p.kind == PickerKind::Session)
                    {
                        picker.items = items;
                        self.ui.notice = "按对话标题或 Session ID 搜索；Enter 打开".into();
                    }
                }
                Err(error) => self.ui.notice = format!("读取会话失败：{error:#}"),
            }
        }
        if let Some(result) = finished_task(&mut self.file_index).await {
            match result {
                Ok(files) => {
                    if let Some(picker) = self
                        .ui
                        .picker
                        .as_mut()
                        .filter(|p| p.kind == PickerKind::File)
                    {
                        picker.items = file_items(&files);
                    }
                    self.files = Some(files);
                    self.ui.notice = "文件引用只插入草稿；发送后由 Agent 读取".into();
                }
                Err(error) => self.ui.notice = format!("文件索引失败：{error:#}"),
            }
        }
        if let Some(result) = finished_task(&mut self.diff).await {
            match result {
                Ok(text) => {
                    if self
                        .ui
                        .detail
                        .as_ref()
                        .is_some_and(|(title, _)| title == "项目修改（只读）")
                    {
                        self.ui.detail = Some(("项目修改（只读）".into(), text));
                    }
                }
                Err(error) => self.ui.notice = format!("修改检查失败：{error:#}"),
            }
        }
        if let Some(result) = finished_task(&mut self.export).await {
            match result {
                Ok(path) => {
                    self.ui.notice = "HTML 导出完成".into();
                    self.detail_event = None;
                    self.ui.open_detail(
                        "HTML 导出完成",
                        format!(
                            "{}\n\n报告包含持久化对话、工具行动和归属信息。可以用浏览器打开这个本地文件。",
                            path.display()
                        ),
                    );
                }
                Err(error) => self.ui.notice = format!("导出失败：{error:#}"),
            }
        }
    }
    async fn command(
        &mut self,
        engine: &mut Engine,
        data: &Path,
        terminal: &mut Terminal,
        input: &mut Option<EventStream>,
        text: &str,
    ) -> Result<bool> {
        let (command, args) = text
            .trim()
            .split_once(char::is_whitespace)
            .unwrap_or((text.trim(), ""));
        let args = args.trim();
        match command {
            "/quit" | "/exit" => return Ok(true),
            "/help" => self.ui.open_help(),
            "/stop" => {
                pause(engine, &mut self.ui)?;
                self.clear_preview();
            }
            "/resume" => resume(engine, &mut self.ui),
            "/new" => {
                self.switch_session(engine, data, None)?;
            }
            "/sessions" => {
                if args.is_empty() {
                    self.sessions(engine, data)?;
                } else {
                    self.switch_session(engine, data, Some(args))?;
                }
            }
            "/model" => {
                if args.is_empty() {
                    self.models();
                } else {
                    self.set_model(engine, args)?;
                }
            }
            "/files" => self.complete_file(engine),
            "/search" => self.start_search(engine, data, args),
            "/questions" => self.questions(engine),
            "/reply" => self.bind_reply(engine, args)?,
            "/message" => self.cancel_reply(),
            "/delivery" => self.open_delivery(engine, data)?,
            "/audit" => self.open_audit(engine)?,
            "/reconcile" => {
                if args.is_empty() {
                    self.reconciliations(engine);
                } else if let Some((call, note)) = args.split_once(char::is_whitespace) {
                    pause(engine, &mut self.ui)?;
                    engine.resolve_write(call, note.trim())?;
                    self.ui.notice = "已记录核查结果；执行仍暂停，Ctrl+R 显式恢复".into();
                } else {
                    self.begin_reconcile(engine, args)?;
                }
            }
            "/mouse" => {
                self.mouse_capture = !self.mouse_capture;
                if self.mouse_capture {
                    execute!(stdout(), EnableMouseCapture)?;
                } else {
                    execute!(stdout(), DisableMouseCapture)?;
                }
                self.ui.notice = if self.mouse_capture {
                    "鼠标滚动已启用；/mouse 切换原生文本选择"
                } else {
                    "终端文本选择已启用；/mouse 恢复鼠标滚动"
                }
                .into();
            }
            "/details" => self.ui.toggle_inspector(),
            "/status" => {
                let state = engine.state();
                let detail = format!(
                    "Session: {}\nWorkspace: {}\nModel: {}\nProfile: {}\nMode: {}\nStatus: {}\n\nLimits\n{}\n\n本次请求用量\n{}\n\n未知写入（需核查实际效果再 reconcile）\n{}",
                    state.id,
                    state.workspace.display(),
                    model_label(&self.settings.profile),
                    self.settings.profile_name,
                    if engine.options().read_only {
                        "只读"
                    } else {
                        "可写"
                    },
                    self.current_status(engine),
                    serde_json::to_string_pretty(engine.options())?,
                    self.last_input
                        .as_ref()
                        .map(|id| {
                            let root = engine
                                .read_event(id)
                                .ok()
                                .and_then(|e| e.root_input)
                                .unwrap_or_else(|| id.clone());
                            engine.metrics(&root)
                        })
                        .unwrap_or(Value::Null),
                    serde_json::to_string_pretty(&state.unknown_writes)?
                );
                self.detail_event = None;
                self.ui.open_detail("会话状态", detail);
            }
            "/diff" => {
                abort_task(&mut self.diff);
                let workspace = engine.state().workspace.clone();
                self.diff = Some(tokio::spawn(
                    async move { services::git_diff(&workspace).await },
                ));
                self.detail_event = None;
                self.ui
                    .open_detail("项目修改（只读）", "正在读取 Git 修改…");
            }
            "/export" => {
                ensure!(self.export.is_none(), "export is already running");
                let data = data.to_owned();
                let id = engine.state().id.clone();
                self.export = Some(tokio::task::spawn_blocking(move || {
                    services::export(&data, &id)
                }));
                self.ui.notice = "正在导出对话和行动记录…".into();
            }
            "/copy" => self.copy_selected(engine).await?,
            "/older" => self.load_older(engine, data)?,
            "/latest" => self.refresh_latest(engine, data)?,
            "/editor" => self.editor(engine, data, terminal, input).await?,
            _ => bail!("未知命令 {command}；Ctrl+P 或 /help 查看命令（未发送给模型）"),
        }
        Ok(false)
    }
    async fn editor(
        &mut self,
        engine: &mut Engine,
        data: &Path,
        terminal: &mut Terminal,
        input: &mut Option<EventStream>,
    ) -> Result<()> {
        let editor = std::env::var("VISUAL")
            .or_else(|_| std::env::var("EDITOR"))
            .context("请设置 VISUAL 或 EDITOR，例如 EDITOR='code --wait'")?;
        ensure!(
            !editor.trim().is_empty() && editor.len() < 4096,
            "invalid editor command"
        );
        self.save(engine, data)?;
        pause(engine, &mut self.ui)?;
        self.clear_preview();
        let path = data
            .join("tui")
            .join(format!("editor-{}.txt", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(path.parent().context("missing editor directory")?)?;
        let mut options = std::fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        {
            use std::io::Write;
            let mut file = options.open(&path)?;
            file.write_all(self.ui.draft().as_bytes())?;
        }
        drop(input.take());
        terminal.suspend();
        // EDITOR is the user's local command; the draft path is passed as a separate argument.
        #[cfg(unix)]
        let mut command = {
            let mut command = tokio::process::Command::new("sh");
            command
                .arg("-c")
                .arg(format!("exec {editor} \"$1\""))
                .arg("bone-editor")
                .arg(&path);
            command
        };
        #[cfg(not(unix))]
        let mut command = {
            let mut command = tokio::process::Command::new(&editor);
            command.arg(&path);
            command
        };
        command.kill_on_drop(true);
        let result = async {
            let mut child = command.spawn().context("start external editor")?;
            #[cfg(unix)]
            let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
            #[cfg(not(unix))] let mut term = ();
            tokio::select! {
                result = child.wait() => result.context("wait for external editor"),
                _ = termination(&mut term) => { child.start_kill()?; bail!("external editor interrupted by SIGTERM; original draft retained") },
                _ = tokio::signal::ctrl_c() => { child.start_kill()?; bail!("external editor interrupted; original draft retained") },
            }
        }.await;
        let restored = terminal.reopen();
        *input = Some(EventStream::new());
        let edited = (|| -> Result<String> {
            ensure!(
                result.context("start external editor")?.success(),
                "external editor failed; original draft retained"
            );
            use std::io::Read;
            let mut text = String::new();
            std::fs::File::open(&path)?
                .take(128 * 1024 + 1)
                .read_to_string(&mut text)?;
            ensure!(
                text.len() <= 128 * 1024,
                "edited input exceeds 128 KiB; original draft retained"
            );
            Ok(text)
        })();
        let _ = std::fs::remove_file(&path);
        restored?;
        if !self.mouse_capture {
            execute!(stdout(), DisableMouseCapture)?;
        }
        let text = edited?;
        self.ui.take_draft();
        self.ui.paste(&text);
        self.ui.notice = "已返回编辑后的草稿，Enter 才发送；工作已暂停".into();
        self.save(engine, data)?;
        Ok(())
    }
}

fn abort_task<T>(slot: &mut Option<tokio::task::JoinHandle<T>>) {
    if let Some(task) = slot.take() {
        task.abort();
    }
}
async fn finished_task<T>(
    slot: &mut Option<tokio::task::JoinHandle<Result<T>>>,
) -> Option<Result<T>> {
    if !slot.as_ref()?.is_finished() {
        return None;
    }
    Some(
        slot.take()
            .unwrap()
            .await
            .context("UI task failed")
            .and_then(|result| result),
    )
}

fn model_label(profile: &Profile) -> String {
    match &profile.model {
        ModelReference::Registry(reference) => reference.to_string(),
        ModelReference::Cohere { model, .. } => format!("cohere:{model}"),
        ModelReference::Ollama { model, .. } => format!("ollama:{model}"),
        ModelReference::Bedrock { bedrock } => format!("bedrock:{bedrock}"),
        ModelReference::VertexAi { vertexai } => format!("vertexai:{vertexai}"),
        ModelReference::Candle { .. } => "candle".into(),
    }
}
fn profile_with_model(current: &Profile, value: &str) -> Result<Profile> {
    use rig_core::providers::registry::{Provider, ProviderRef};
    let mut next = Profile::from_model(value)?;
    let preserved = match (&current.model, &next.model) {
        (ModelReference::Registry(old), ModelReference::Registry(new))
            if old.id() == new.id() && old.id().is_some() =>
        {
            let reference = match old.provider() {
                Provider::Registered(id) => ProviderRef::registered(*id, new.model())?,
                Provider::Configured(config) => {
                    ProviderRef::configured(config.clone(), new.model())?
                }
            };
            Some(ModelReference::Registry(reference))
        }
        (ModelReference::Cohere { cohere, .. }, ModelReference::Cohere { model, .. }) => {
            Some(ModelReference::Cohere {
                cohere: cohere.clone(),
                model: model.clone(),
            })
        }
        (ModelReference::Ollama { ollama, .. }, ModelReference::Ollama { model, .. }) => {
            Some(ModelReference::Ollama {
                ollama: ollama.clone(),
                model: model.clone(),
            })
        }
        (ModelReference::Bedrock { .. }, ModelReference::Bedrock { .. })
        | (ModelReference::VertexAi { .. }, ModelReference::VertexAi { .. }) => {
            Some(next.model.clone())
        }
        _ => None,
    };
    if let Some(model) = preserved {
        next = current.clone();
        next.model = model;
    }
    // A change of provider uses that provider's own credential recipe. A model
    // change within a provider preserves its endpoint, dialect, and auth source.
    next.validate()?;
    Ok(next)
}
const COMMANDS: &[(&str, &str)] = &[
    ("/new", "新对话，当前工作保存并暂停"),
    ("/sessions", "搜索并打开当前项目的会话"),
    ("/model", "选择模型配置；/model 原生名称可覆盖当前模型"),
    ("/status", "模型、执行限制、用量与未确认写入"),
    ("/diff", "查看 Git 修改，包括 staged 与 unstaged"),
    ("/files", "插入项目文件引用"),
    ("/search", "搜索整个会话的持久原文；Ctrl+F"),
    ("/questions", "显式选择回复目标；Esc 返回原阅读层"),
    ("/message", "返回新要求草稿，明确切换发送目标"),
    ("/delivery", "直达最近持久交付及其输入来源"),
    ("/audit", "查看当前持久结果的审计原文"),
    ("/reply", "指定问题 ID 作为回复目标"),
    ("/reconcile", "核查未知写入并记录实际结果"),
    ("/mouse", "切换鼠标滚动和终端原生文本选择"),
    ("/older", "向前翻页，保留当前阅读位置"),
    ("/latest", "返回最近对话窗口，保留草稿"),
    ("/export", "导出本地 HTML 对话与行动报告"),
    ("/editor", "用 VISUAL / EDITOR 编辑长输入"),
    ("/copy", "复制当前草稿、选区、详情或明确选中的持久对象"),
    ("/details", "展开或隐藏内部事件记录"),
    ("/stop", "暂停所有工作"),
    ("/resume", "恢复暂停工作"),
    ("/help", "键盘帮助"),
    ("/quit", "保存并退出"),
];
fn command_items() -> Vec<PickerItem> {
    COMMANDS
        .iter()
        .map(|&(label, detail)| PickerItem {
            label: label.into(),
            detail: detail.into(),
            value: label.into(),
        })
        .collect()
}
fn file_items(files: &[String]) -> Vec<PickerItem> {
    files
        .iter()
        .map(|path| PickerItem {
            label: path.clone(),
            detail: "项目文件 · 仅插入引用".into(),
            value: path.clone(),
        })
        .collect()
}
fn session_items(data: &Path, workspace: &Path, current: &str) -> Result<Vec<PickerItem>> {
    let mut items = Vec::new();
    for session in bone::sessions(data)?
        .into_iter()
        .filter(|s| s.workspace == workspace)
        .take(200)
    {
        let first = bone::history_page(data, &session.id, None, 1)?;
        let last = bone::history_before(data, &session.id, None, 1)?;
        let title = first
            .events
            .first()
            .filter(|e| e.data["source"] == "user")
            .map(|e| {
                native_text(&e.data["message"])
                    .lines()
                    .next()
                    .unwrap_or("")
                    .chars()
                    .take(80)
                    .collect::<String>()
            })
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "新对话".into());
        let stamp = last
            .events
            .last()
            .and_then(|e| e.timestamp.parse::<u64>().ok())
            .unwrap_or(0);
        items.push((
            stamp,
            PickerItem {
                label: format!(
                    "{title}{}",
                    if session.id == current {
                        " · 当前"
                    } else {
                        ""
                    }
                ),
                detail: format!(
                    "{} · {} · {} 轮",
                    session.id,
                    if session.paused {
                        "已暂停"
                    } else {
                        "已保存"
                    },
                    session.revision
                ),
                value: session.id,
            },
        ));
    }
    items.sort_by_key(|(stamp, _)| std::cmp::Reverse(*stamp));
    Ok(items.into_iter().map(|(_, item)| item).collect())
}
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}
async fn copy_to_clipboard(text: &str) -> Result<()> {
    use std::process::Stdio;
    use tokio::io::AsyncWriteExt;
    let choices: &[(&str, &[&str])] = if cfg!(target_os = "macos") {
        &[("pbcopy", &[])]
    } else {
        &[
            ("wl-copy", &[]),
            ("xclip", &["-selection", "clipboard"]),
            ("xsel", &["--clipboard", "--input"]),
        ]
    };
    tokio::time::timeout(Duration::from_secs(3), async {
        for &(program, args) in choices {
            let mut child = match tokio::process::Command::new(program)
                .args(args)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn()
            {
                Ok(child) => child,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            let mut stdin = child.stdin.take().context("missing clipboard input")?;
            stdin.write_all(text.as_bytes()).await?;
            drop(stdin);
            ensure!(
                child.wait().await?.success(),
                "系统剪贴板不可用；可以使用 /export"
            );
            return Ok(());
        }
        bail!("未找到 pbcopy、wl-copy、xclip 或 xsel；可以使用 /export")
    })
    .await
    .context("clipboard command timed out")?
}
fn pause(engine: &mut Engine, ui: &mut View) -> Result<()> {
    if !engine.state().paused {
        engine.stop()?;
    }
    ui.notice = "工作已暂停；Ctrl+R 恢复，Ctrl+Q 退出".into();
    Ok(())
}
fn resume(engine: &mut Engine, ui: &mut View) {
    match engine.resume() {
        Ok(()) => ui.notice = "已恢复；可以继续补充要求".into(),
        Err(error) => ui.notice = format!("无法恢复：{error:#}"),
    }
}

pub(super) async fn run(
    engine: &mut Engine,
    data: &Path,
    settings: Settings,
    seconds: u64,
) -> Result<()> {
    let mut terminal = Terminal::open()?;
    let mut app = App::new(settings);
    let result = event_loop(&mut terminal, engine, data, &mut app, seconds).await;
    let saved = app.save(engine, data);
    let stopped = if engine.state().paused {
        Ok(())
    } else {
        engine.stop()
    };
    abort_task(&mut app.diff);
    abort_task(&mut app.file_index);
    abort_task(&mut app.session_index);
    abort_task(&mut app.search);
    abort_task(&mut app.export);
    drop(terminal);
    eprintln!(
        "Session: {}\n继续：bone --data-dir {} --profile {} --model {} tui --session {} --workspace {}{}\n打开后 Ctrl+R 恢复暂停的工作。",
        engine.state().id,
        shell_quote(&data.to_string_lossy()),
        shell_quote(&app.settings.profile_name),
        shell_quote(&model_label(&app.settings.profile)),
        shell_quote(&engine.state().id),
        shell_quote(&engine.state().workspace.to_string_lossy()),
        if engine.options().read_only {
            " --read-only"
        } else {
            ""
        }
    );
    result?;
    saved?;
    stopped
}
async fn event_loop(
    terminal: &mut Terminal,
    engine: &mut Engine,
    data: &Path,
    app: &mut App,
    seconds: u64,
) -> Result<()> {
    app.load(engine, data)?;
    let mut input = Some(EventStream::new());
    let duration = Duration::from_secs(seconds.max(1));
    let mut deadline = (!engine.is_quiescent()).then(|| Instant::now() + duration);
    let mut cooldown = Instant::now();
    let mut dirty = true;
    let mut tick = tokio::time::interval(Duration::from_millis(50));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    #[cfg(unix)]
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    #[cfg(not(unix))]
    let mut terminate = ();
    loop {
        if dirty {
            app.metadata(engine);
            let model = model_label(&app.settings.profile);
            let fact = app.current_status(engine);
            terminal
                .terminal
                .draw(|frame| app.ui.render(frame, engine.state(), &model, &fact))?;
            dirty = false;
        }
        if engine.is_quiescent() {
            deadline = None;
        }
        tokio::select! {
            biased;
            event = next_terminal(&mut input) => {
                let revision = engine.state().revision;
                match event? {
                    TerminalEvent::Key(key) if key.kind != KeyEventKind::Release => {
                        let control = key.modifiers.contains(KeyModifiers::CONTROL);
                        match key.code {
                            KeyCode::Char('q') if control => return Ok(()),
                            KeyCode::Char('c') if control => {
                                pause(engine, &mut app.ui)?; app.clear_preview(); deadline = None;
                            },
                            KeyCode::Char('y') if control => { if let Err(error) = app.copy_selected(engine).await { app.ui.notice = format!("复制失败：{error:#}"); } },
                            KeyCode::Char('f') if control => app.start_search(engine, data, ""),
                            KeyCode::Esc if app.reconcile_editor_active() => { app.handle_reconcile_key(engine, key)?; },
                            KeyCode::Char('d') if control && app.reconcile_call.is_some() => {
                                if let Some(call) = app.reconcile_call.clone() {
                                    match reconciliation_detail(engine, &call) {
                                        Ok(text) => { app.detail_event = engine.read_call_event(&call, "tool_result").or_else(|_| engine.read_call_event(&call, "tool_started")).ok().map(|e| e.id); app.ui.open_detail(format!("核查证据 · {}", short_id(&call)), text); app.remember_detail_source(); },
                                        Err(error) => app.ui.notice = format!("读取核查证据失败：{error:#}"),
                                    }
                                }
                            },
                            KeyCode::Char('D') if !control && (app.ui.detail.is_some() || app.ui.focus == Focus::Conversation) => {
                                if let Err(error) = app.open_audit(engine) { app.ui.notice = format!("读取审计失败：{error:#}"); }
                            },
                            KeyCode::Char('d') if app.ui.focus == Focus::Conversation && !app.ui.has_modal() => {
                                if let Err(error) = app.open_selected(engine) { app.ui.notice = format!("读取失败：{error:#}"); }
                            },
                            KeyCode::Char('y') if app.ui.focus == Focus::Conversation && !app.ui.has_modal() => {
                                if let Err(error) = app.copy_selected(engine).await { app.ui.notice = format!("复制失败：{error:#}"); }
                            },
                            KeyCode::Char('r') if control && app.reconcile_call.is_some() => app.ui.notice = "先记录或取消核查；执行仍暂停".into(),
                            KeyCode::Char('r') if control => { resume(engine, &mut app.ui); deadline = Some(Instant::now()+duration); },
                            KeyCode::Char('p') if control => app.commands(String::new()),
                            KeyCode::Char('o') if control => app.complete_file(engine),
                            KeyCode::Char('g') if control => {
                                if let Err(error) = app.editor(engine, data, terminal, &mut input).await { app.ui.notice = format!("编辑失败：{error:#}"); }
                            },
                            KeyCode::Tab | KeyCode::Enter if app.ui.is_completion() && app.ui.picker_value().is_some()
                                && !key.modifiers.intersects(KeyModifiers::ALT | KeyModifiers::CONTROL | KeyModifiers::SHIFT) => {
                                if let Some((kind, value)) = app.ui.picker_value() { app.complete_inline(kind, &value); }
                            },
                            KeyCode::Enter if app.ui.picker.is_some() && !app.ui.is_completion() => {
                                if let Some((kind, value)) = app.ui.picker_value() {
                                    app.ui.close_layer();
                                    let result = match kind {
                                        PickerKind::Command => {
                                            if app.reconcile_call.is_some() && !matches!(value.as_str(), "/copy" | "/help" | "/stop" | "/status" | "/audit" | "/diff" | "/quit") {
                                                Err(anyhow::anyhow!("先记录或取消当前核查表单；原草稿仍保留"))
                                            } else {
                                                app.command(engine, data, terminal, &mut input, &value).await
                                            }
                                        },
                                        PickerKind::Session => app.switch_session(engine, data, Some(&value)).map(|_|false),
                                        PickerKind::Model => app.set_model(engine, &value).map(|_|false),
                                        PickerKind::History => {
                                            app.ui.select_message(&value);
                                            engine.read_event(&value).and_then(|e| detail(engine, &e)).map(|text| {
                                                app.detail_event = Some(value.clone()); app.ui.open_detail(format!("搜索原文 · {} · {} · /audit 审计", app.search_query, short_id(&value)), text); app.remember_detail_source(); false
                                            })
                                        },
                                        PickerKind::Question => app.bind_reply(engine, &value).map(|_| false),
                                        PickerKind::Reconcile => app.begin_reconcile(engine, &value).map(|_| false),
                                        PickerKind::File => {
                                            let reference = if value.contains(char::is_whitespace) { format!("@\"{value}\" ") } else { format!("@{value} ") };
                                            if let Some(range) = app.file_range.take() { app.ui.replace_range(range, &reference); }
                                            else { app.ui.paste(&reference); }
                                            Ok(false)
                                        },
                                    };
                                    match result { Ok(true) => return Ok(()), Ok(false) => {}, Err(error) => app.ui.notice = format!("操作失败：{error:#}") }
                                }
                            },
                            _ if app.ui.has_modal() => app.ui.handle_key(key),
                            KeyCode::Enter if app.reconcile_editor_active()
                                && !key.modifiers.intersects(KeyModifiers::ALT | KeyModifiers::CONTROL | KeyModifiers::SHIFT) => {
                                if let Err(error) = app.handle_reconcile_key(engine, key) { app.ui.notice = format!("记录失败：{error:#}；核查草稿保留"); }
                            },
                            KeyCode::Enter if !key.modifiers.intersects(KeyModifiers::ALT | KeyModifiers::CONTROL | KeyModifiers::SHIFT) => {
                                match app.ui.focus {
                                    Focus::Input => {
                                        let text = app.ui.draft().clone();
                                        if !text.trim().is_empty() {
                                            app.ui.close_completion();
                                            if text.trim_start().starts_with('/') {
                                                // Keep an unknown command in the editor; do not turn it into agent input.
                                                let command_draft = app.ui.draft_snapshot();
                                                app.ui.take_draft();
                                                let result = app.command(engine, data, terminal, &mut input, &text).await;
                                                match result {
                                                    Ok(true) => return Ok(()),
                                                    Ok(false) => {},
                                                    Err(error) => { app.ui.restore_draft(command_draft); app.ui.notice = format!("操作失败：{error:#}；草稿已保留"); },
                                                }
                                            } else {
                                                let posted = if let Some(target) = app.reply_target.as_deref() { engine.post(&text, Some(target)) } else { engine.post_message(&text) };
                                                match posted {
                                                    Ok(id) => { app.ui.remember_prompt(&text); app.ui.take_draft(); app.clear_preview();
                                                        let sent_target = app.reply_target.clone();
                                                        app.drafts.remove(&sent_target.clone().unwrap_or_default());
                                                        if sent_target.is_some() { app.switch_target(None); }
                                                        app.last_input = Some(id.clone());
                                                        app.ui.notice = format!("输入 {} 已接收 · 等待纳入执行", short_id(&id));
                                                        app.elapsed_from = Instant::now(); deadline = Some(Instant::now()+duration); },
                                                    Err(error) => app.ui.notice = format!("发送失败：{error:#}；草稿已保留"),
                                                }
                                            }
                                        }
                                    },
                                    Focus::Activity => {
                                        if let Some(id) = app.ui.selected_event().map(str::to_owned) {
                                            match engine.read_event(&id).and_then(|e| detail(engine, &e)) {
                                                Ok(text) => { app.detail_event = Some(id.clone()); app.ui.open_detail(format!("行动原文 · {} · /audit 审计", short_id(&id)), text); app.remember_detail_source(); },
                                                Err(error) => app.ui.notice = format!("读取记录失败：{error:#}"),
                                            }
                                        }
                                    },
                                    Focus::Conversation => app.ui.toggle_selected_message(),
                                }
                            },
                            _ => app.ui.handle_key(key),
                        }
                        app.refresh_search(engine, data);
                        if let Err(error) = app.refresh_completion(engine, data) { app.ui.notice = format!("补全失败：{error:#}"); }
                        app.changed_at = Some(Instant::now()); dirty = true;
                    },
                    TerminalEvent::Paste(text) => { app.ui.handle_paste(&text); app.refresh_search(engine, data); app.refresh_completion(engine, data)?; app.changed_at = Some(Instant::now()); dirty = true; },
                    TerminalEvent::Mouse(mouse) => { app.ui.handle_mouse(mouse); dirty = true; },
                    TerminalEvent::Resize(..) => dirty = true,
                    _ => {},
                }
                if engine.state().revision != revision {
                    app.clear_preview(); app.sync(engine, data)?;
                    if !engine.is_quiescent() && deadline.is_none() { deadline = Some(Instant::now()+duration); }
                }
            },
            _ = termination(&mut terminate) => return Ok(()),
            _ = tokio::signal::ctrl_c() => { pause(engine,&mut app.ui)?; app.clear_preview(); deadline = None; app.sync(engine,data)?; dirty = true; },
            _ = tokio::time::sleep_until(deadline.unwrap_or_else(|| Instant::now()+duration)), if deadline.is_some() => {
                pause(engine,&mut app.ui)?; app.clear_preview(); deadline = None;
                app.ui.notice = "运行时限已到，已暂停；Ctrl+R 继续".into(); app.sync(engine,data)?; dirty = true;
            },
            _ = tick.tick(), if !engine.is_quiescent() || app.has_tasks() || app.changed_at.is_some() || app.notice_until.is_some() => {
                let pending = app.has_tasks();
                app.tasks().await;
                dirty |= app.progress(engine) || !engine.is_quiescent() || pending || app.expire_notice();
                if app.changed_at.is_some_and(|at| at.elapsed() >= Duration::from_millis(500))
                    && let Err(error) = app.save(engine,data) {
                    app.ui.notice = format!("草稿保存失败：{error:#}"); app.changed_at = None; dirty = true;
                }
            },
            _ = tokio::time::sleep_until(cooldown), if Instant::now() < cooldown => {},
            updates = engine.step(), if !engine.is_quiescent() && Instant::now() >= cooldown => {
                match updates {
                    Ok(events) => {
                        if events.is_empty() { cooldown = Instant::now()+Duration::from_millis(25); }
                        app.progress(engine); app.sync(engine,data)?; dirty = true;
                    },
                    Err(error) => return Err(error.context("TUI session execution failed")),
                }
            },
        }
    }
}
#[cfg(unix)]
async fn termination(signal: &mut tokio::signal::unix::Signal) {
    signal.recv().await;
}
#[cfg(not(unix))]
async fn termination(_: &mut ()) {
    std::future::pending::<()>().await;
}
async fn next_terminal(input: &mut Option<EventStream>) -> Result<TerminalEvent> {
    input
        .as_mut()
        .context("terminal input suspended")?
        .next()
        .await
        .context("terminal input closed")?
        .map_err(Into::into)
}

fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

fn terminal_label(kind: &str) -> &str {
    match kind {
        "delivery" => "已交付",
        "failure" => "执行失败",
        "input_paused" => "已暂停",
        "input_resolved" => "已结算",
        _ => "等待处理",
    }
}

fn is_complete_command(text: &str) -> bool {
    COMMANDS.iter().any(|(command, _)| *command == text)
        && !matches!(
            text,
            "/reply" | "/model" | "/sessions" | "/search" | "/reconcile"
        )
}

fn current_status(engine: &Engine, input: Option<&str>, tool_failure: Option<&str>) -> String {
    let state = engine.state();
    if !state.unknown_writes.is_empty() {
        return format!("需核查 {} 项写入 · /reconcile", state.unknown_writes.len());
    }
    if state.paused {
        return "已暂停 · Ctrl+R 恢复".into();
    }
    let terminal = input.and_then(|id| engine.result(id)).filter(|e| {
        matches!(
            e.kind.as_str(),
            "delivery" | "failure" | "input_paused" | "input_resolved"
        )
    });
    if let Some(event) = terminal {
        return format!(
            "输入 {} {}{}",
            short_id(input.unwrap_or("")),
            terminal_label(&event.kind),
            if event.kind == "delivery" {
                " · /delivery 查看"
            } else {
                ""
            }
        );
    }
    if !state.pending_inputs.is_empty() {
        return format!(
            "{} 条输入已接收 · 等待纳入执行{}",
            state.pending_inputs.len(),
            tool_failure.map(|_| " · 有工具失败记录").unwrap_or("")
        );
    }
    let questions = engine.unanswered_questions().len();
    let running = state
        .jobs
        .values()
        .filter(|job| job.state == JobState::Running)
        .count();
    let activity = if questions > 0 {
        format!("{questions} 个问题待回答 · /questions")
    } else if running > 0 {
        format!("工作中 · {running} 项正在执行")
    } else if !engine.is_quiescent() {
        "正在安排工作".into()
    } else if state
        .jobs
        .values()
        .any(|job| job.state == JobState::Waiting)
    {
        "等待工作结果".into()
    } else {
        "暂无活动调用".into()
    };
    if let Some(failure) = tool_failure {
        format!("最近工具失败 {failure} · {activity}")
    } else {
        activity
    }
}

fn raw_tool_output(engine: &Engine, event: &Event) -> Result<String> {
    let text = engine.event_text(event)?;
    Ok(if text.is_empty() {
        native_text(&event.data["message"])
    } else {
        text
    })
}

fn tool_failed(engine: &Engine, event: &Event) -> Result<bool> {
    if event.data["uncertain"] == true {
        return Ok(false);
    }
    let value = serde_json::from_str::<Value>(&raw_tool_output(engine, event)?).ok();
    Ok(event.data["error"].is_string()
        || value.as_ref().is_some_and(|v| {
            v["error"].is_string() || v["exit_code"].as_i64().is_some_and(|code| code != 0)
        }))
}

fn question_summary(question: &str, max_width: usize) -> String {
    let source = question.replace(['\n', '\r'], " ");
    let mut width = 0;
    let mut result = String::new();
    for character in source.chars() {
        let size = unicode_width::UnicodeWidthChar::width(character).unwrap_or(0);
        if width + size > max_width {
            result.push('…');
            break;
        }
        width += size;
        result.push(character);
    }
    result
}

fn failure_marker(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    if lower.contains("...")
        && ["ok", "ignored", "skipped", "expected failure"]
            .iter()
            .any(|ending| lower.trim_end().ends_with(ending))
    {
        return false;
    }
    lower.contains("traceback")
        || lower.contains("错误")
        || lower.contains("失败")
        || lower
            .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .any(|word| {
                matches!(
                    word,
                    "fail" | "failed" | "failure" | "error" | "panic" | "panicked"
                ) || word.ends_with("error")
            })
}

fn tool_failure_observation(
    engine: &Engine,
    event: &Event,
    body: &str,
) -> Result<(Option<i64>, String)> {
    let raw = raw_tool_output(engine, event)?;
    let value = serde_json::from_str::<Value>(&raw).ok();
    let exit = value.as_ref().and_then(|v| v["exit_code"].as_i64());
    let stderr = value
        .as_ref()
        .and_then(|v| v["stderr"].as_str())
        .unwrap_or("");
    let stdout = value
        .as_ref()
        .and_then(|v| v["stdout"].as_str())
        .unwrap_or("");
    let error = value
        .as_ref()
        .and_then(|v| v["error"].as_str())
        .or_else(|| event.data["error"].as_str());
    let observed = format!("{stderr}\n{stdout}");
    let lines: Vec<_> = observed
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    if let Some(line) = lines
        .iter()
        .find(|line| {
            failure_marker(line)
                && !line
                    .trim_start()
                    .to_ascii_lowercase()
                    .starts_with("traceback")
        })
        .or_else(|| lines.iter().find(|line| failure_marker(line)))
    {
        return Ok((exit, line.trim().into()));
    }
    if let Some(error) = error {
        return Ok((exit, format!("工具报告：{error}")));
    }
    let stream = if !stderr.trim().is_empty() {
        Some(("stderr", stderr))
    } else if !stdout.trim().is_empty() {
        Some(("stdout", stdout))
    } else {
        None
    };
    if let Some((name, text)) = stream {
        let tail = text
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("");
        return Ok((exit, format!("{name}尾部观察：{}", tail.trim())));
    }
    let lines: Vec<_> = body
        .lines()
        .filter(|line| {
            !line.trim().is_empty()
                && *line != "stderr:"
                && *line != "stdout:"
                && !line.starts_with("exit:")
        })
        .collect();
    let observation = lines
        .iter()
        .find(|line| failure_marker(line))
        .map(|line| line.trim().to_owned())
        .unwrap_or_else(|| "工具报告失败；查看完整结果".into());
    Ok((exit, observation))
}

fn tool_summary(engine: &Engine, event: &Event, body: &str) -> Result<String> {
    let raw = raw_tool_output(engine, event)?;
    let value = serde_json::from_str::<Value>(&raw).ok();
    let result = if let Some(value) = value {
        if let Some(exit) = value["exit_code"].as_i64() {
            format!(
                "exit {exit} · stdout {} 行 · stderr {} 行",
                value["stdout"].as_str().unwrap_or("").lines().count(),
                value["stderr"].as_str().unwrap_or("").lines().count()
            )
        } else if let Some(entries) = value["entries"].as_array() {
            format!("返回 {} 项", entries.len())
        } else if let Some(matches) = value["matches"].as_array() {
            format!("返回 {} 处匹配", matches.len())
        } else if let Some(bytes) = value["bytes"].as_u64() {
            format!("返回 {bytes} 字节")
        } else {
            format!("返回 {} 行", body.lines().count())
        }
    } else {
        format!("返回 {} 行", body.lines().count())
    };
    let target = tool_arguments(engine, event)?.and_then(|args| {
        args["path"]
            .as_str()
            .or_else(|| args["command"].as_str())
            .or_else(|| args["query"].as_str())
            .map(|s| s.replace('\n', " ").chars().take(20).collect::<String>())
    });
    Ok(format!(
        "{}{result} · {}",
        target.map(|t| format!("{t} · ")).unwrap_or_default(),
        short_id(event.call_id.as_deref().unwrap_or(&event.id))
    ))
}

fn reconciliation_detail(engine: &Engine, call: &str) -> Result<String> {
    let event = engine
        .read_call_event(call, "tool_result")
        .or_else(|_| engine.read_call_event(call, "tool_started"))?;
    Ok(format!(
        "调用 {call}\n实际效果未确认；检查文件、Git diff、进程或工具实际结果后，在核查表单记录观察。\n\n{}",
        detail(engine, &event)?
    ))
}

fn tool_arguments(engine: &Engine, event: &Event) -> Result<Option<Value>> {
    let Some((source_id, call_key)) = event.data["tool_key"]
        .as_str()
        .and_then(|key| key.split_once(':'))
    else {
        return Ok(None);
    };
    let call_id: Value = serde_json::from_str(call_key)?;
    let source = engine.read_event(source_id)?;
    Ok(source.data["response"]["choice"]
        .as_array()
        .and_then(|choices| {
            choices
                .iter()
                .find(|choice| choice["type"] == "toolcall" && choice["id"] == call_id)
        })
        .map(|choice| choice["function"]["arguments"].clone()))
}

fn audit_detail(event: &Event) -> Result<String> {
    Ok(format!(
        "event: {}\nkind: {}\njob: {}\ncall: {}\nreply_to: {}\nroot_input: {}\nrevision: {}\ntimestamp (Unix ms): {}\n\n持久事件原文\n{}",
        event.id,
        event.kind,
        event.job_id.as_deref().unwrap_or("—"),
        event.call_id.as_deref().unwrap_or("—"),
        event.reply_to.as_deref().unwrap_or("—"),
        event.root_input.as_deref().unwrap_or("—"),
        event.revision,
        event.timestamp,
        serde_json::to_string_pretty(&event.data)?
    ))
}

fn detail(engine: &Engine, event: &Event) -> Result<String> {
    let mut output = event_body(engine, event)?;
    if output.is_empty() {
        output = "此记录没有正文；/audit 查看持久事件与审计元信息".into();
    }
    if let Some(arguments) = tool_arguments(engine, event)? {
        let preview =
            proposed_change_preview(event.data["tool_name"].as_str().unwrap_or(""), &arguments);
        if !preview.is_empty() {
            output.push_str(&format!("\n\n提议修改（以实际结果为准）\n{preview}"));
        }
        output.push_str(&format!(
            "\n\n原生工具参数\n{}",
            serde_json::to_string_pretty(&arguments)?
        ));
    }
    if event.kind == "model_message" {
        let calls: Vec<_> = event.data["response"]["choice"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|choice| choice["type"] == "toolcall")
            .collect();
        if !calls.is_empty() {
            output.push_str(&format!(
                "\n\n原生工具提议\n{}",
                serde_json::to_string_pretty(&calls)?
            ));
        }
    }
    Ok(output)
}

fn event_body(engine: &Engine, event: &Event) -> Result<String> {
    let mut text = engine.event_text(event)?;
    if text.is_empty() {
        text = native_text(&event.data["message"]);
    }
    if text.is_empty() {
        text = native_text(&event.data["response"]["choice"]);
    }
    if event.kind == "tool_result" {
        text = readable_tool_output(&text);
    }
    Ok(text)
}

fn readable_tool_output(text: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return text.into();
    };
    if let Some(error) = value["error"].as_str() {
        return format!(
            "错误：{error}\n{}",
            if value["effect"] == "unknown" {
                "结果未知，需要核查实际效果"
            } else {
                ""
            }
        );
    }
    if value.get("stdout").is_some() || value.get("stderr").is_some() {
        let stdout = value["stdout"].as_str().unwrap_or("");
        let stderr = value["stderr"].as_str().unwrap_or("");
        let error_output = if stderr.is_empty() {
            String::new()
        } else {
            format!("\nstderr:\n{stderr}")
        };
        if value["exit_code"].as_i64().is_some_and(|code| code != 0) && !stderr.trim().is_empty() {
            return format!(
                "stderr:\n{stderr}\nexit: {}\nstdout:\n{stdout}",
                value["exit_code"]
            );
        }
        return format!("exit: {}\n{stdout}{error_output}", value["exit_code"]);
    }
    if let Some(body) = value["text"].as_str() {
        return body.into();
    }
    if let Some(entries) = value["entries"].as_array() {
        return entries
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join("\n");
    }
    if let Some(matches) = value["matches"].as_array() {
        return matches
            .iter()
            .map(|m| {
                format!(
                    "{}:{}  {}",
                    m["path"].as_str().unwrap_or(""),
                    m["line"],
                    m["snippet"].as_str().unwrap_or("")
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
    }
    text.into()
}

fn proposed_change_preview(name: &str, args: &Value) -> String {
    let mut lines = Vec::new();
    if name == "edit_file" {
        for edit in args["edits"].as_array().into_iter().flatten().take(8) {
            lines.push("@@".to_owned());
            for (field, prefix) in [("old_text", '-'), ("new_text", '+')] {
                let text = edit[field].as_str().unwrap_or("");
                for line in text.lines().take(12) {
                    lines.push(format!("{prefix} {line}"));
                }
                if text.lines().count() > 12 {
                    lines.push(format!("{prefix} [更多内容保留在原生参数]"));
                }
            }
        }
    } else if name == "write_file"
        && let Some(content) = args["content"].as_str()
    {
        let mut preview = content.lines().take(12).collect::<Vec<_>>().join("\n");
        if content.lines().count() > 12 {
            preview.push_str("\n[更多内容保留在原生参数]");
        }
        return format!(
            "拟写入内容（可能覆盖现有文件）\n```text\n{}\n```",
            preview.chars().take(2000).collect::<String>()
        );
    }
    if lines.is_empty() {
        return String::new();
    }
    format!(
        "```diff\n{}\n```",
        lines.join("\n").chars().take(2000).collect::<String>()
    )
}

fn ingest(engine: &Engine, event: &Event, ui: &mut View) -> Result<()> {
    let public = event
        .reply_to
        .as_deref()
        .map(|id| {
            engine
                .read_event(id)
                .map(|input| input.data["source"] == "user")
        })
        .transpose()?
        .unwrap_or(false);
    let user = event.kind == "input" && event.data["source"] == "user";
    let body = event_body(engine, event)?;
    let arguments = tool_arguments(engine, event)?;
    if user
        || event.kind == "question"
        || (public && matches!(event.kind.as_str(), "delivery" | "failure" | "input_paused"))
    {
        let role = if user {
            "你 · 已接收"
        } else if event.kind == "question" {
            "Agent · 提问"
        } else if event.kind == "failure" {
            "执行失败"
        } else {
            "Agent"
        };
        ui.push_message(Message {
            summary: None,
            role: if user {
                format!("{role} · {}", short_id(&event.id))
            } else {
                role.into()
            },
            text: body.clone(),
            event_id: Some(event.id.clone()),
        });
    }
    if event.kind == "input_handled"
        && let Some(id) = event.data["input_id"].as_str()
    {
        let input = engine.read_event(id)?;
        if input.data["source"] == "user" {
            ui.upsert_message(Message {
                role: format!("你 · 已纳入 · {}", short_id(id)),
                text: event_body(engine, &input)?,
                event_id: Some(id.into()),
                summary: None,
            });
        }
    }
    let job = event
        .job_id
        .as_deref()
        .and_then(|id| engine.state().jobs.get(id))
        .map(|job| job.title.as_str())
        .unwrap_or("Session");
    let name = event.data["tool_name"].as_str().unwrap_or("tool");
    // Engineering actions belong in the main transcript. Job routing stays in
    // the optional inspector; a user never has to select an internal Job.
    if matches!(event.kind.as_str(), "tool_started" | "tool_result")
        && !name.starts_with("job_")
        && !matches!(name, "ask_user" | "question")
    {
        let label = arguments
            .as_ref()
            .and_then(|args| args["path"].as_str().or(args["command"].as_str()))
            .map(|text| text.chars().take(180).collect::<String>())
            .unwrap_or_default();
        let result = if event.kind == "tool_result" {
            body.as_str()
        } else {
            ""
        };
        let failed = tool_failed(engine, event)?;
        let phase = if event.kind == "tool_started" {
            "执行中"
        } else if event.data["uncertain"] == true {
            "结果未知 · 需核查 /reconcile"
        } else if failed {
            "失败"
        } else {
            "完成"
        };
        let failure = if failed {
            Some(tool_failure_observation(engine, event, result)?)
        } else {
            None
        };
        let text = if let Some((_, observation)) = &failure {
            format!("{phase}\n{observation}\n{label}\n{result}")
        } else {
            format!(
                "{}{}{}\n{}",
                phase,
                if label.is_empty() { "" } else { " · " },
                label,
                result
            )
        };
        let summary = if let Some((exit, observation)) = failure {
            Some(format!(
                "失败{} · {observation}",
                exit.map(|code| format!(" · exit {code}"))
                    .unwrap_or_default()
            ))
        } else if event.kind == "tool_result" && event.data["uncertain"] != true {
            Some(tool_summary(engine, event, result)?)
        } else {
            None
        };
        let changes = arguments
            .as_ref()
            .map(|args| proposed_change_preview(name, args))
            .unwrap_or_default();
        let text = if changes.is_empty() {
            text
        } else {
            format!("{text}\n提议修改（以工具执行结果为准）\n{changes}")
        };
        ui.upsert_message(Message {
            summary,
            role: format!("工具 · {name}"),
            text,
            event_id: Some(format!(
                "tool:{}",
                event.call_id.as_deref().unwrap_or(&event.id)
            )),
        });
    }
    if event.kind == "model_message"
        && event.revision == engine.state().revision
        && event.job_id.as_ref() == engine.state().focus.as_ref()
        && event.data["response"]["choice"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item["type"] == "toolcall"))
    {
        let text = native_text(&event.data["response"]["choice"]);
        if !text.is_empty() {
            ui.push_message(Message {
                summary: None,
                role: "Agent · 进度".into(),
                text,
                event_id: Some(event.id.clone()),
            });
        }
    }
    let (title, tone) = match event.kind.as_str() {
        "input" if user => ("收到你的输入".into(), Tone::Info),
        "input" => ("Job 收到内部工作".into(), Tone::Info),
        "model_started" if event.data["purpose"] == "summary" => ("压缩上下文".into(), Tone::Info),
        "model_started" => ("调用模型 · 思考工作".into(), Tone::Info),
        "model_message" => {
            let names: Vec<_> = event.data["response"]["choice"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|choice| choice["type"] == "toolcall")
                .filter_map(|choice| choice["function"]["name"].as_str())
                .collect();
            (
                if names.is_empty() {
                    "模型返回".into()
                } else {
                    format!("准备调用 {}", names.join(" / "))
                },
                Tone::Normal,
            )
        }
        "tool_started" => (format!("执行 {name}"), Tone::Info),
        "tool_result" => (
            format!("{name} 返回"),
            if event.data.get("error").is_some() {
                Tone::Error
            } else {
                Tone::Normal
            },
        ),
        "summary" => (
            format!(
                "已压缩 {} 条历史",
                event.data["covered_ids"].as_array().map_or(0, Vec::len)
            ),
            Tone::Success,
        ),
        "question" => ("向你提问".into(), Tone::Info),
        "delivery" => ("本轮交付".into(), Tone::Success),
        "failure" | "model_failed" => ("执行失败".into(), Tone::Error),
        "cancelled" => ("调用已取消".into(), Tone::Normal),
        "stopped" | "input_paused" => ("工作已暂停".into(), Tone::Info),
        "resumed" => ("工作已恢复".into(), Tone::Info),
        "input_resolved" => ("旧输入已结算".into(), Tone::Success),
        "input_handled" => ("已纳入新指令".into(), Tone::Normal),
        "stale" => ("过期结果仅入历史".into(), Tone::Normal),
        _ => (event.kind.clone(), Tone::Normal),
    };
    let preview = if let Some(arguments) = arguments {
        format!("参数 {}\n{}", serde_json::to_string(&arguments)?, body)
    } else if !body.is_empty() {
        body
    } else {
        format!("{} · event {}", event.kind, event.id)
    };
    let preview: String = preview.chars().take(500).collect();
    ui.push_activity(Activity {
        event_id: event.id.clone(),
        title: format!("{job} · {title}"),
        detail: preview,
        tone,
    });
    Ok(())
}

#[cfg(test)]
#[path = "../tests/unit/tui.rs"]
mod tests;
