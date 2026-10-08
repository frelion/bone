//! Terminal presentation of the Session interface. Execution remains owned by Engine.
mod services;
mod view;

use anyhow::{Context, Result, bail, ensure};
use bone::config::{Config, ConfigRevision, ModelReference, Profile, validate_profile_name};
use bone::runtime::Engine;
use bone::state::{Event, JobState};
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event as TerminalEvent, EventStream, KeyCode, KeyEventKind, KeyModifiers,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use futures_util::{StreamExt, stream::FuturesUnordered};
use ratatui::DefaultTerminal;
use rig_core::providers::chatgpt::auth::{DeviceCodeHandler, DeviceCodePrompt};
use serde_json::Value;
use services::native_text;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{IsTerminal, stdout};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::time::Instant;
use view::{
    Activity, Draft, Focus, FormField, InputTarget, Message, MessageKind, PickerItem, PickerKind,
    SessionItem, Tone, ToolState, View,
};

/// Concrete launch recipes, not a second runtime or model abstraction.
#[derive(Clone)]
pub(super) struct Settings {
    pub profile_name: String,
    pub profile: Profile,
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

struct ActiveCall {
    phase: &'static str,
    target: String,
    started: Instant,
}

// One local editing transaction. Its revision protects against overwriting an
// externally edited config; the form alone owns any unsaved secret text.
#[derive(Clone)]
struct ConnectionChange {
    config: Config,
    revision: ConfigRevision,
    name: String,
    profile: Profile,
}
#[derive(Clone)]
enum ConnectionForm {
    Model,
    Api,
    Subscription,
}
#[derive(Clone)]
struct ConnectionSetup {
    change: ConnectionChange,
    form: ConnectionForm,
}
struct CredentialReservation {
    data: PathBuf,
    name: String,
}
impl Drop for CredentialReservation {
    fn drop(&mut self) {
        cleanup_connection(&self.data, &self.name);
    }
}

struct App {
    ui: View,
    cursor: Option<String>,
    older: Option<String>,
    settings: Settings,
    default_settings: Settings,
    parts: BTreeMap<String, BTreeMap<u64, String>>,
    last_input: Option<String>,
    last_admitted_input: Option<String>,
    active_calls: BTreeMap<String, ActiveCall>,
    last_saved: services::UiSaved,
    changed_at: Option<Instant>,
    elapsed_from: Instant,
    file_range: Option<std::ops::Range<usize>>,
    file_index: Option<tokio::task::JoinHandle<Result<Vec<String>>>>,
    files: Option<Vec<String>>,
    session_index: Option<tokio::task::JoinHandle<Result<Vec<SessionItem>>>>,
    connection_setup: Option<ConnectionSetup>,
    login: Option<tokio::task::JoinHandle<Result<(ConnectionChange, CredentialReservation)>>>,
    login_codes: Option<tokio::sync::mpsc::Receiver<DeviceCodePrompt>>,
    diff: Option<tokio::task::JoinHandle<Result<String>>>,
    export: Option<tokio::task::JoinHandle<Result<PathBuf>>>,
    exported_report: Option<PathBuf>,
    search: Option<tokio::task::JoinHandle<Result<Vec<PickerItem>>>>,
    search_query: String,
    completion_key: Option<(String, usize)>,
    tool_previews: BTreeMap<String, Message>,
    tool_observations: BTreeMap<String, (String, String)>,
    reply_target: Option<String>,
    drafts: BTreeMap<String, Draft>,
    background: BTreeMap<String, Engine>,
    recipes: BTreeMap<String, Settings>,
    deadlines: BTreeMap<String, Instant>,
    duration: Duration,
    detail_event: Option<String>,
    detail_sources: BTreeMap<String, String>,
    last_tool_failure: Option<String>,
    exit_requested: bool,
}
impl App {
    fn new(settings: Settings) -> Self {
        Self {
            ui: View::new(),
            cursor: None,
            older: None,
            default_settings: settings.clone(),
            settings,
            parts: BTreeMap::new(),
            last_input: None,
            last_admitted_input: None,
            active_calls: BTreeMap::new(),
            last_saved: services::UiSaved::default(),
            changed_at: None,
            elapsed_from: Instant::now(),
            file_range: None,
            file_index: None,
            files: None,
            session_index: None,
            connection_setup: None,
            login: None,
            login_codes: None,
            diff: None,
            export: None,
            exported_report: None,
            search: None,
            search_query: String::new(),
            completion_key: None,
            tool_previews: BTreeMap::new(),
            tool_observations: BTreeMap::new(),
            reply_target: None,
            drafts: BTreeMap::new(),
            background: BTreeMap::new(),
            recipes: BTreeMap::new(),
            deadlines: BTreeMap::new(),
            duration: Duration::from_secs(300),
            detail_event: None,
            detail_sources: BTreeMap::new(),
            last_tool_failure: None,
            exit_requested: false,
        }
    }
    fn maintain_execution(&mut self, current: &mut Engine) -> Result<()> {
        let now = Instant::now();
        for engine in std::iter::once(&mut *current).chain(self.background.values_mut()) {
            let id = engine.state().id.clone();
            if engine.is_quiescent() || engine.state().paused {
                self.deadlines.remove(&id);
            } else {
                let deadline = self
                    .deadlines
                    .entry(id.clone())
                    .or_insert(now + self.duration);
                if now >= *deadline {
                    engine.stop()?;
                    self.deadlines.remove(&id);
                    self.ui.notify("会话运行时限已到；执行正在停止");
                }
            }
        }
        self.background.retain(|_, engine| !engine.is_quiescent());
        Ok(())
    }
    fn execution_update(
        &mut self,
        current: &mut Engine,
        data: &Path,
        id: &str,
        result: Result<Vec<Event>>,
    ) -> Result<()> {
        if id == current.state().id {
            result.context("TUI session execution failed")?;
            self.progress(current);
            self.sync(current, data)?;
        } else {
            if let Err(error) = result {
                self.background.remove(id);
                self.ui
                    .fail(format!("后台会话 {} 执行失败：{error:#}", short_id(id)));
            }
            self.refresh_sessions(current, data);
        }
        Ok(())
    }
    fn load(&mut self, engine: &mut Engine, data: &Path) -> Result<()> {
        let (name, profile) = engine.profile_recipe();
        self.settings = Settings {
            profile_name: name.into(),
            profile: profile.clone(),
        };
        self.ui = View::new();
        self.parts.clear();
        self.tool_previews.clear();
        self.tool_observations.clear();
        self.reply_target = None;
        self.drafts.clear();
        self.detail_event = None;
        self.detail_sources.clear();
        self.last_tool_failure = None;
        self.completion_key = None;
        self.search_query.clear();
        abort_task(&mut self.search);
        self.last_input = None;
        self.last_admitted_input = None;
        self.active_calls.clear();
        self.cursor = None;
        self.older = None;
        self.files = None;
        self.file_range = None;
        abort_task(&mut self.file_index);
        abort_task(&mut self.session_index);
        self.cancel_login();
        self.connection_setup = None;
        abort_task(&mut self.diff);
        abort_task(&mut self.export);
        self.exported_report = None;
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
        self.older = more.then(|| ids.last().cloned()).flatten();
        self.cursor = ids.first().cloned();
        for id in ids.iter().rev() {
            self.ingest(engine, &engine.read_event(id)?)?;
        }
        for question in engine.unanswered_questions() {
            if !ids.contains(&question.id) {
                self.ingest(engine, &engine.read_event(&question.id)?)?;
            }
        }
        match services::load(data, &engine.state().id) {
            Ok(saved) => {
                if !saved.history.is_empty() {
                    self.ui.set_history(saved.history.clone());
                }
                self.ui.paste(&saved.draft);
                self.ui
                    .restore_input_position(Some(saved.cursor), saved.selection);
                self.reply_target = saved.reply_to.clone();
                self.drafts = saved
                    .drafts
                    .iter()
                    .filter(|(target, _)| {
                        target.as_str() != saved.reply_to.as_deref().unwrap_or("")
                    })
                    .map(|(target, draft)| (target.clone(), view::Draft::restored(draft)))
                    .collect();
                self.last_saved = saved;
                if !self.ui.draft().is_empty() {
                    self.ui.notify("已恢复未发送的草稿；Enter 才发送");
                }
            }
            Err(error) => {
                self.last_saved = services::UiSaved::default();
                self.ui.fail(format!("草稿恢复失败：{error:#}"));
            }
        }
        self.changed_at = None;
        self.elapsed_from = Instant::now();
        engine.drain_model_progress();
        engine.drain_tool_progress();
        self.refresh_usage(engine);
        self.metadata(engine);
        self.refresh_sessions(engine, data);
        Ok(())
    }
    fn refresh_latest(&mut self, engine: &mut Engine, data: &Path) -> Result<()> {
        self.save(engine, data)?;
        let active = self.ui.draft_snapshot();
        let inactive = std::mem::take(&mut self.drafts);
        let target = self.reply_target.clone();
        let admitted = self.last_admitted_input.clone();
        let focus = self.ui.focus;
        let main_focus = self.ui.main_focus;
        let refreshed = self.load(engine, data);
        self.ui.restore_draft(active);
        self.ui.focus = focus;
        self.ui.main_focus = main_focus;
        self.drafts = inactive;
        self.reply_target = target;
        if admitted == self.last_input {
            self.last_admitted_input = admitted;
        }
        self.metadata(engine);
        refreshed?;
        self.ui
            .notify("已返回最新记录；草稿、光标、选区与回复目标已保留");
        Ok(())
    }
    fn save(&mut self, engine: &Engine, data: &Path) -> Result<()> {
        let (cursor, selection) = self.ui.input_position();
        let saved = services::UiSaved {
            draft: self.ui.draft(),
            history: self.ui.history(),
            reply_to: self.reply_target.clone(),
            cursor,
            selection,
            drafts: self
                .drafts
                .iter()
                .map(|(target, draft)| (target.clone(), draft.saved()))
                .collect(),
        };
        if saved != self.last_saved {
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
            self.last_admitted_input = self.last_input.clone();
            self.ui.notify("新要求已纳入执行");
        }
        if event.kind == "input" && event.data["source"] == "user" {
            self.last_input = Some(event.id.clone());
            self.last_admitted_input = None;
            self.ui
                .remember_prompt(&native_text(&event.data["message"]));
        }
        if self.last_input.is_none() {
            self.last_input = event.root_input.clone();
        }
        if matches!(
            event.kind.as_str(),
            "model_message"
                | "model_failed"
                | "model_limited"
                | "summary"
                | "model_cancelled"
                | "cancelled"
        ) && let Some(call) = &event.call_id
        {
            self.ui.remove_message(&format!("live:{call}"));
            self.parts.remove(call);
        }
        if matches!(event.kind.as_str(), "tool_started" | "tool_result")
            && let Some(call) = &event.call_id
        {
            self.tool_previews.remove(call);
        }
        if event.kind == "question" && engine.is_unanswered_question(event) {
            self.ui.notify("有问题待回答 · Ctrl+P 选择回复");
        }
        if event.kind == "model_limited" && event.data["retry_allowed"] == true {
            self.ui.notify("本轮生成未完成，正在继续");
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
        let mut refresh_sessions = false;
        loop {
            let page = bone::history_page(data, &engine.state().id, self.cursor.as_deref(), 20)?;
            for event in &page.events {
                refresh_sessions |= matches!(
                    event.kind.as_str(),
                    "delivery" | "question" | "stopped" | "resumed" | "failure"
                );
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
        if refresh_sessions {
            self.refresh_sessions(engine, data);
        }
        Ok(())
    }
    fn metadata(&mut self, engine: &Engine) {
        let questions = engine.unanswered_questions();
        self.ui.input_target = if self.reply_target.is_some() {
            InputTarget::Reply
        } else {
            InputTarget::Message
        };
        self.ui.reply_label = if let Some(id) = &self.reply_target {
            if let Some(question) = questions.iter().find(|q| &q.id == id) {
                format!(
                    "回复：{}",
                    question_summary(question.data["question"].as_str().unwrap_or("问题"), 80)
                )
            } else {
                "回复目标已失效 · 草稿保留".into()
            }
        } else {
            String::new()
        };
        self.cache_active_calls(engine);
        self.ui.busy = execution_active(engine.state());
        self.ui.spinner_tick = if self.ui.busy {
            (self.elapsed_from.elapsed().as_millis() / 140) as usize
        } else {
            0
        };
        self.ui.session_label = engine.state().id.chars().take(8).collect();
        self.ui.live_status = self.current_status(engine);
        self.ui.feedback_detail = self.feedback_detail(engine);
    }
    fn cache_active_calls(&mut self, engine: &Engine) {
        self.active_calls.retain(|call, _| {
            engine.state().jobs.values().any(|job| {
                job.state == JobState::Running && job.current_call.as_ref() == Some(call)
            })
        });
        for job in engine
            .state()
            .jobs
            .values()
            .filter(|job| job.state == JobState::Running)
        {
            if let Some(call) = &job.current_call
                && !self.active_calls.contains_key(call)
            {
                let feedback = active_call(engine, call).unwrap_or(ActiveCall {
                    phase: "正在执行",
                    target: String::new(),
                    started: Instant::now(),
                });
                self.active_calls.insert(call.clone(), feedback);
            }
        }
    }
    fn selected_active_call(&self, engine: &Engine) -> Option<(&str, &ActiveCall)> {
        let preferred = engine
            .state()
            .focus
            .as_ref()
            .and_then(|id| engine.state().jobs.get(id))
            .filter(|job| job.state == JobState::Running)
            .and_then(|job| job.current_call.as_ref())
            .and_then(|call| self.active_calls.get_key_value(call));
        preferred
            .or_else(|| {
                engine
                    .state()
                    .jobs
                    .values()
                    .filter(|job| job.state == JobState::Running)
                    .filter_map(|job| job.current_call.as_ref())
                    .find_map(|call| self.active_calls.get_key_value(call))
            })
            .map(|(id, feedback)| (id.as_str(), feedback))
    }
    fn feedback_detail(&self, engine: &Engine) -> String {
        if !self.ui.notice.is_empty() {
            return self.ui.notice.clone();
        }
        if self
            .last_input
            .as_ref()
            .is_some_and(|id| engine.state().pending_inputs.contains(id))
        {
            return if engine.state().paused {
                "新要求已接收 · 暂停等待恢复"
            } else {
                "新要求已接收 · 等待纳入执行"
            }
            .into();
        }
        if let Some((call, feedback)) = self.selected_active_call(engine) {
            if let Some(failure) = &self.last_tool_failure {
                return format!("最近工具失败 {failure} · 当前执行仍在继续");
            }
            if feedback.phase == "执行命令" {
                return if self
                    .tool_observations
                    .get(call)
                    .is_some_and(|(out, err)| !out.is_empty() || !err.is_empty())
                {
                    "正在接收命令输出"
                } else {
                    "命令尚未产生输出；执行仍在继续"
                }
                .into();
            }
            if self
                .parts
                .get(call)
                .is_some_and(|parts| parts.values().any(|text| !text.is_empty()))
            {
                return "正在接收输出".into();
            }
            if self.last_admitted_input == self.last_input && self.last_input.is_some() {
                return "新要求已纳入 · 正在处理".into();
            }
        }
        if engine.state().paused && !engine.unanswered_questions().is_empty() {
            return "待答问题仍保留 · Ctrl+P 回复问题".into();
        }
        if let Some(failure) = &self.last_tool_failure {
            return format!("最近工具失败 {failure}");
        }
        String::new()
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
                kind: MessageKind::Streaming,
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
                kind: MessageKind::Tool {
                    name: progress.tool_name,
                    state: ToolState::Running,
                },
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
    fn commands(&mut self, engine: &Engine, query: String) {
        let mut items = command_items();
        if !engine.unanswered_questions().is_empty() {
            items.push(action("回复问题", "/questions", "选择等待中的问题"));
        }
        if self.reply_target.is_some() {
            items.push(action("写新要求", "/message", "保存回复草稿，回到新要求"));
        }

        items.push(action("查看项目修改", "/diff", "Git 修改 · Ctrl+D"));
        items.push(action("导出对话记录", "/export", "保存当前对话与行动记录"));
        if self.exported_report.is_some() {
            items.push(action("查看导出报告", "/export show", "报告文件的完整路径"));
        }
        self.ui.open_picker(PickerKind::Command, items, query);
    }
    fn refresh_sessions(&mut self, engine: &Engine, data: &Path) {
        abort_task(&mut self.session_index);
        self.ui.mark_active_session(&engine.state().id);
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        self.ui.sessions_loading = true;
        self.ui.sessions_error = None;
        let data = data.to_owned();
        let workspace = engine.state().workspace.clone();
        let current = engine.state().id.clone();
        self.session_index = Some(tokio::task::spawn_blocking(move || {
            session_items(&data, &workspace, &current)
        }));
    }
    fn model_form(&mut self, data: &Path) -> Result<()> {
        let (config, revision) = Config::load_with_revision(data)?;
        let profile = self.settings.profile.clone();
        self.ui.open_form(
            format!("模型 · {}", self.settings.profile_name),
            vec![field(
                "模型名称 · 服务商的模型 ID",
                model_name(&profile),
                false,
            )],
        );
        self.connection_setup = Some(ConnectionSetup {
            change: ConnectionChange {
                config,
                revision,
                name: self.settings.profile_name.clone(),
                profile,
            },
            form: ConnectionForm::Model,
        });
        Ok(())
    }
    fn connections(&mut self, data: &Path) -> Result<()> {
        let config = Config::load(data)?;
        let mut items = Vec::new();
        for (name, profile) in &config.profiles {
            let credentials = if profile.is_subscription() {
                if bone::has_login(profile, data, name)? {
                    "登录信息已保存"
                } else {
                    "需要登录"
                }
            } else if bone::has_api_key(data, name)? {
                "API key 已保存"
            } else {
                "环境变量 / 原生认证"
            };
            items.push(PickerItem {
                label: format!(
                    "{name}{}",
                    if *name == self.settings.profile_name {
                        " · 当前"
                    } else {
                        ""
                    }
                ),
                detail: format!("{} · {credentials}", model_label(profile)),
                value: name.clone(),
            });
        }
        items.extend([
            action(
                "ChatGPT · 使用现有 Codex 登录",
                "@codex",
                "复用本机登录，凭据不复制",
            ),
            action("ChatGPT · 登录", "@login", "浏览器设备登录，BONE 独立保存"),
            action(
                "添加 API 连接",
                "@api",
                "选择 provider、模型、endpoint 与凭据",
            ),
        ]);
        self.ui
            .open_picker(PickerKind::Connection, items, String::new());
        Ok(())
    }
    fn connection_choice(&mut self, engine: &mut Engine, data: &Path, value: &str) -> Result<()> {
        if value == "@api" {
            let items = bone::providers()
                .into_iter()
                .filter(|provider| {
                    Profile::from_model(&format!("{provider}:model")).is_ok_and(|profile| {
                        !profile.is_subscription() && profile.endpoint().is_some()
                    })
                })
                .map(|provider| {
                    let (vendor, protocol) =
                        provider.split_once('/').unwrap_or((&provider, &provider));
                    PickerItem {
                        label: vendor.into(),
                        detail: if vendor == protocol {
                            "API 模型".into()
                        } else {
                            format!("API 模型 · {protocol} 协议")
                        },
                        value: format!("@api/{provider}"),
                    }
                })
                .collect();
            self.ui
                .open_picker(PickerKind::Connection, items, String::new());
            return Ok(());
        }
        let (config, revision) = Config::load_with_revision(data)?;
        let (form, suggested, profile) = match value {
            "@codex" | "@login" => {
                let reuse = value == "@codex";
                let mut profile = Profile::from_model("chatgpt:model")?;
                profile.reuse_codex_login = reuse;
                (ConnectionForm::Subscription, "chatgpt".to_owned(), profile)
            }
            _ if value.starts_with("@api/") => {
                let provider = value.trim_start_matches("@api/");
                (
                    ConnectionForm::Api,
                    provider
                        .split('/')
                        .next()
                        .unwrap_or(provider)
                        .replace('-', "_"),
                    Profile::from_model(&format!("{provider}:model"))?,
                )
            }
            _ => {
                let profile = config.profile(Some(value))?.clone();
                return self.apply_connection(
                    engine,
                    data,
                    ConnectionChange {
                        config,
                        revision,
                        name: value.into(),
                        profile,
                    },
                );
            }
        };
        let name = unused_connection_name(&config, data, &suggested);
        let model = if profile.provider_identity() == self.settings.profile.provider_identity() {
            model_name(&self.settings.profile)
        } else {
            String::new()
        };
        let mut fields = vec![
            field("连接名称 · 字母、数字、_ 或 -", name.clone(), false),
            field("模型名称 · 原生名称", model, false),
        ];
        let title = match &form {
            ConnectionForm::Api => {
                fields.push(field(
                    "API endpoint · 完整 URL",
                    profile.endpoint().unwrap_or_default(),
                    false,
                ));
                fields.push(field(
                    "API key · 留空使用环境变量或原生认证",
                    String::new(),
                    true,
                ));
                format!(
                    "添加连接 · {}",
                    model_provider(&profile).split('/').next().unwrap_or("API")
                )
            }
            ConnectionForm::Subscription => if profile.reuse_codex_login {
                "ChatGPT · 现有登录"
            } else {
                "ChatGPT · 设备登录"
            }
            .into(),
            ConnectionForm::Model => unreachable!(),
        };
        self.ui.open_form(title, fields);
        self.connection_setup = Some(ConnectionSetup {
            change: ConnectionChange {
                config,
                revision,
                name,
                profile,
            },
            form,
        });
        Ok(())
    }
    fn submit_connection_form(&mut self, engine: &mut Engine, data: &Path) -> Result<()> {
        ensure!(self.ui.form_is_editable(), "扩大窗口后编辑和保存；Esc 返回");
        let values = self.ui.form_values().context("连接表单已关闭")?;
        let (step, _) = self.ui.form_step().context("连接表单已关闭")?;
        let setup = self
            .connection_setup
            .as_ref()
            .context("连接表单已失效，请重新打开")?;
        let value = values[step].trim();
        match (&setup.form, step) {
            (ConnectionForm::Model, 0) => {
                setup.change.profile.with_model(&format!(
                    "{}:{value}",
                    model_provider(&setup.change.profile)
                ))?;
            }
            (_, 0) => {
                validate_profile_name(value)?;
                ensure!(
                    !setup.change.config.profiles.contains_key(value)
                        && !data.join("profiles").join(value).exists(),
                    "连接名称已存在，请换一个名称"
                );
            }
            (_, 1) => {
                Profile::from_model(&format!(
                    "{}:{value}",
                    model_provider(&setup.change.profile)
                ))?;
            }
            (ConnectionForm::Api, 2) => {
                setup.change.profile.with_endpoint(value)?;
            }
            (ConnectionForm::Api, 3) => ensure!(
                !values[step].chars().any(char::is_control),
                "API key 必须是单行文字"
            ),
            _ => {}
        }
        if !self.ui.advance_form() {
            return Ok(());
        }
        let mut setup = self.connection_setup.take().context("连接表单已关闭")?;
        let original = setup.clone();
        let result = (|| -> Result<()> {
            if matches!(setup.form, ConnectionForm::Model) {
                setup.change.profile = setup.change.profile.with_model(&format!(
                    "{}:{}",
                    model_provider(&setup.change.profile),
                    values[0].trim()
                ))?;
                self.apply_connection(engine, data, setup.change)?;
            } else {
                setup.change.name = values[0].trim().into();
                validate_profile_name(&setup.change.name)?;
                ensure!(
                    !setup
                        .change
                        .config
                        .profiles
                        .contains_key(&setup.change.name),
                    "连接名称已存在，请换一个名称"
                );
                setup.change.profile = setup.change.profile.with_model(&format!(
                    "{}:{}",
                    model_provider(&setup.change.profile),
                    values[1].trim()
                ))?;
                if matches!(setup.form, ConnectionForm::Api) {
                    setup.change.profile = setup.change.profile.with_endpoint(values[2].trim())?;
                }
                // Reserve a fresh credential directory. Another setup cannot
                // overwrite the key while our config transaction is pending.
                bone::has_api_key(data, &setup.change.name)?; // also rejects credential path symlinks
                std::fs::create_dir_all(data.join("profiles"))?;
                std::fs::create_dir(data.join("profiles").join(&setup.change.name))
                    .context("连接名称已被使用，请重新打开连接列表")?;
                let name = setup.change.name.clone();
                let reservation = CredentialReservation {
                    data: data.to_owned(),
                    name: name.clone(),
                };
                match setup.form {
                    ConnectionForm::Api => {
                        if !values[3].trim().is_empty() {
                            bone::save_api_key(
                                data,
                                &name,
                                &setup.change.profile,
                                values[3].trim(),
                            )?;
                        }
                        self.apply_connection(engine, data, setup.change)?;
                    }
                    ConnectionForm::Subscription => {
                        self.ui.close_layer();
                        self.ui.open_detail(
                            "登录 ChatGPT",
                            "正在读取登录信息…\n\nEsc 取消，当前连接和草稿保留。",
                        );
                        let (sender, receiver) = tokio::sync::mpsc::channel(1);
                        let handler = DeviceCodeHandler::new(move |prompt| {
                            let _ = sender.try_send(prompt);
                        });
                        let data = data.to_owned();
                        self.login_codes = Some(receiver);
                        self.login = Some(tokio::spawn(async move {
                            bone::login_with(
                                &setup.change.profile,
                                &data,
                                &setup.change.name,
                                rig_core::http_client::DynHttpClient::new(rig_reqwest::shared()),
                                handler,
                            )
                            .await?;
                            Ok((setup.change, reservation))
                        }));
                    }
                    ConnectionForm::Model => unreachable!(),
                }
            }
            Ok(())
        })();
        if result.is_ok() && self.ui.form_is_open() {
            self.ui.close_layer();
        }
        // A failed transaction keeps all form editors intact. Reopen loads a
        // fresh revision rather than silently replaying a conflicting save.
        if result.is_err() {
            self.connection_setup = Some(original);
        }
        result
    }
    fn apply_connection(
        &mut self,
        engine: &mut Engine,
        data: &Path,
        mut change: ConnectionChange,
    ) -> Result<()> {
        change.profile.validate()?;
        change
            .config
            .profiles
            .insert(change.name.clone(), change.profile.clone());
        change.config.default_profile = change.name.clone();
        let old_name = self.settings.profile_name.clone();
        let old_profile = self.settings.profile.clone();
        engine.set_profile(change.profile.clone(), change.name.clone())?;
        let mut warning = None;
        if let Err(error) = change.config.save_checked(data, &change.revision) {
            let installed = Config::load(data).ok().is_some_and(|saved| {
                serde_json::to_value(saved).ok() == serde_json::to_value(&change.config).ok()
            });
            if !installed {
                engine.set_profile(old_profile, old_name)?;
                return Err(error);
            }
            warning = Some(format!("配置已安装，磁盘同步失败：{error:#}"));
        }
        self.settings.profile_name = change.name;
        self.settings.profile = change.profile;
        self.default_settings = self.settings.clone();
        self.recipes
            .insert(engine.state().id.clone(), self.settings.clone());
        if let Some(warning) = warning {
            self.ui.fail(warning);
        } else {
            self.ui.notify(
                if engine.state().paused && has_unfinished_work(engine.state()) {
                    "连接与模型已保存 · 首次请求验证认证 · Ctrl+R 继续工作"
                } else {
                    "连接与模型已保存 · 下次请求使用新模型"
                },
            );
        }
        Ok(())
    }
    fn cancel_login(&mut self) {
        abort_task(&mut self.login);
        self.login_codes = None;
    }
    fn start_diff(&mut self, engine: &Engine) {
        abort_task(&mut self.diff);
        let workspace = engine.state().workspace.clone();
        self.diff = Some(tokio::spawn(
            async move { services::git_diff(&workspace).await },
        ));
        self.detail_event = None;
        self.ui
            .open_detail("项目修改（只读）", "正在读取 Git 修改…");
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
            self.ui.notify("正在索引项目文件…");
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
        let sidebar = self.ui.focus == Focus::Sessions;
        let return_focus = self.ui.main_focus;
        self.save(engine, data)?;
        // Open first: a bad target must not change or cancel the current session.
        let next = if let Some(next) = id.and_then(|id| self.background.remove(id)) {
            next
        } else {
            let recipe = id
                .and_then(|id| self.recipes.get(id))
                .unwrap_or(&self.default_settings);
            Engine::open(
                data,
                &engine.state().workspace,
                id,
                recipe.profile.clone(),
                recipe.profile_name.clone(),
                engine.options().clone(),
            )?
        };
        self.clear_preview();
        let old_id = engine.state().id.clone();
        self.recipes.insert(old_id.clone(), self.settings.clone());
        let previous = std::mem::replace(engine, next);
        if !previous.is_quiescent() {
            self.background.insert(old_id, previous);
        } else {
            self.deadlines.remove(&old_id);
        }
        self.drafts.clear();
        self.load(engine, data)?;
        self.ui.select_session(&engine.state().id);
        if sidebar && id.is_some() {
            self.ui.focus = Focus::Sessions;
            self.ui.main_focus = return_focus;
        }
        self.ui.notify(
            if id.is_some() && engine.state().paused && has_unfinished_work(engine.state()) {
                "会话已打开 · 已暂停，Ctrl+R 继续"
            } else if id.is_some() {
                "会话已打开"
            } else {
                "新会话已创建；直接描述任务"
            },
        );
        Ok(())
    }
    fn set_model(&mut self, engine: &mut Engine, data: &Path, value: &str) -> Result<()> {
        let (config, revision) = Config::load_with_revision(data)?;
        let native = if Profile::from_model(value).is_ok() {
            value.to_owned()
        } else {
            format!("{}:{value}", model_provider(&self.settings.profile))
        };
        let profile = self.settings.profile.with_model(&native)?;
        ensure!(
            profile.provider_identity() == self.settings.profile.provider_identity(),
            "切换 provider 请使用 /connect"
        );
        self.apply_connection(
            engine,
            data,
            ConnectionChange {
                config,
                revision,
                name: self.settings.profile_name.clone(),
                profile,
            },
        )
    }
    fn refresh_completion(&mut self, engine: &Engine) -> Result<()> {
        let draft = self.ui.draft();
        let cursor = self.ui.cursor();
        let key = (draft.clone(), cursor);
        if self.completion_key.as_ref() == Some(&key) {
            return Ok(());
        }
        self.completion_key = Some(key);
        if self.ui.focus != Focus::Input || self.ui.has_modal() {
            return Ok(());
        }
        let prefix = &draft[..cursor];
        if is_complete_command(prefix) && cursor == draft.len() {
            self.ui.close_completion();
        } else if draft.starts_with('/') && !prefix.contains(char::is_whitespace) {
            let items = command_items();
            if items.iter().any(|item| item.value.starts_with(prefix)) {
                self.ui
                    .open_completion(PickerKind::Command, items, prefix.to_owned());
            } else {
                self.ui.close_completion();
            }
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
        self.ui.notify("已补全；Enter 才执行或发送");
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
        self.ui.notify("正在搜索完整会话原文…");
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
                detail: if self.reply_target.as_ref() == Some(&q.id) {
                    "当前回复目标".into()
                } else {
                    String::new()
                },
                value: q.id.clone(),
            })
            .collect();
        self.ui
            .open_picker(PickerKind::Question, items, String::new());
        self.ui.notify("选择要回答的问题；Esc 返回，草稿保留");
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
        if self.ui.draft().is_empty() {
            self.drafts.remove(&previous);
        } else {
            self.drafts.insert(previous, self.ui.draft_snapshot());
        }
        let next = target.clone().unwrap_or_default();
        let draft = self.drafts.remove(&next).unwrap_or_else(View::empty_draft);
        self.ui.restore_draft(draft);
        self.reply_target = target;
        self.ui.focus = Focus::Input;
    }
    fn bind_reply(&mut self, engine: &Engine, id: &str) -> Result<()> {
        ensure!(
            engine.unanswered_questions().iter().any(|q| q.id == id),
            "回复目标已失效；草稿和原目标保留，Ctrl+P 查看待答问题"
        );
        self.switch_target(Some(id.to_owned()));
        self.ui.notice.clear(); // The persistent target strip confirms the selection.
        Ok(())
    }
    fn cancel_reply(&mut self) {
        self.switch_target(None);
        self.ui.notify("已切换新要求草稿；Enter 发送新要求");
    }
    fn current_status(&self, engine: &Engine) -> String {
        if !engine.state().paused
            && let Some((call, feedback)) = self.selected_active_call(engine)
        {
            let phase = if feedback.phase == "思考中"
                && self
                    .parts
                    .get(call)
                    .is_some_and(|parts| parts.values().any(|text| !text.is_empty()))
            {
                "输出中"
            } else {
                feedback.phase
            };
            let count = engine
                .state()
                .jobs
                .values()
                .filter(|job| job.state == JobState::Running && job.current_call.is_some())
                .count();
            let parallel = if count > 1 {
                format!(" · {count} 项并行")
            } else {
                String::new()
            };
            return format!(
                "{phase}{} · {}s{parallel}",
                if feedback.target.is_empty() {
                    String::new()
                } else {
                    format!(" · {}", feedback.target)
                },
                feedback.started.elapsed().as_secs()
            );
        }
        current_status(
            engine,
            self.last_input.as_deref(),
            self.last_tool_failure.as_deref(),
        )
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
        let observation = self.selected_tool_observation().map(|m| m.text.clone());
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
    fn selected_tool_observation(&self) -> Option<&Message> {
        self.ui.selected_message().filter(|message| {
            matches!(
                message.kind,
                MessageKind::Tool {
                    state: ToolState::Running,
                    ..
                }
            ) && message
                .event_id
                .as_deref()
                .and_then(|id| id.strip_prefix("tool:"))
                .is_some_and(|call| self.tool_observations.contains_key(call))
        })
    }
    async fn copy_selected(&mut self, engine: &Engine) -> Result<()> {
        ensure!(
            !self.ui.form_is_open(),
            "连接表单不复制凭据；Esc 返回原草稿后复制"
        );
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
                let label = if self.reply_target.is_some() {
                    "回复草稿"
                } else {
                    "新要求草稿"
                };
                (label.into(), self.ui.draft())
            }
        } else if self.ui.focus == Focus::Conversation && self.selected_tool_observation().is_some()
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
        self.ui.notify(format!("已复制{object}完整文字"));
        Ok(())
    }
    fn load_older(&mut self, engine: &Engine, data: &Path) -> Result<()> {
        let Some(before) = self.older.as_deref() else {
            self.ui.notify("已经是最早的记录");
            return Ok(());
        };
        let page = bone::history_before(data, &engine.state().id, Some(before), 20)?;
        if page.events.is_empty() {
            self.older = None;
            self.ui.notify("已经是最早的记录");
            return Ok(());
        }
        let mut earlier = View::new();
        for event in &page.events {
            ingest(engine, event, &mut earlier)?;
        }
        self.older = page.has_more.then_some(page.next_cursor).flatten();
        self.ui.prepend_messages(earlier.messages);
        self.ui.notify(if page.has_more {
            "已载入更早对话"
        } else {
            "已载入最早对话"
        });
        Ok(())
    }
    fn has_tasks(&self) -> bool {
        self.file_index.is_some()
            || self.session_index.is_some()
            || self.diff.is_some()
            || self.export.is_some()
            || self.search.is_some()
            || self.login.is_some()
    }
    async fn tasks(&mut self, engine: &mut Engine, data: &Path) {
        if let Some(prompt) = self
            .login_codes
            .as_mut()
            .and_then(|codes| codes.try_recv().ok())
            && self.login.is_some()
        {
            self.ui.detail = Some((
                "登录 ChatGPT".into(),
                format!(
                    "打开：{}\n\n输入验证码：{}\n\n正在等待浏览器登录。\nEsc 取消，当前连接和草稿保留。",
                    prompt.verification_uri, prompt.user_code
                ),
            ));
        }
        if let Some(result) = finished_task(&mut self.login).await {
            self.login_codes = None;
            self.ui.close_layer();
            match result {
                Ok((change, _reservation)) => {
                    if let Err(error) = self.apply_connection(engine, data, change) {
                        self.ui.open_detail("连接未保存", format!("{error:#}\n\n当前连接和草稿保留。Esc 返回后重新打开 /connect。"));
                    }
                },
                Err(_) => self.ui.open_detail("登录未完成", "当前连接和草稿保留。\n检查本机 Codex 登录，或通过 /connect 重新登录。\n\nEsc 返回。"),
            }
        }
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
                        self.ui.notify("搜索完整持久记录；Enter 阅读原文，Esc 返回");
                    }
                }
                Err(error) => self.ui.fail(format!("搜索失败：{error:#}")),
            }
        }
        if let Some(result) = finished_task(&mut self.session_index).await {
            self.ui.sessions_loading = false;
            match result {
                Ok(items) => {
                    self.ui.set_sessions(items.clone());
                    if let Some(picker) = self
                        .ui
                        .picker
                        .as_mut()
                        .filter(|p| p.kind == PickerKind::Session)
                    {
                        picker.items = items
                            .into_iter()
                            .map(|item| PickerItem {
                                label: item.label,
                                detail: item.detail,
                                value: item.value,
                            })
                            .collect();
                    }
                }
                Err(_) => self.ui.sessions_error = Some("会话读取失败 · 已有列表保留".into()),
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
                    self.ui.notify("文件引用只插入草稿；发送后读取文件");
                }
                Err(error) => self.ui.fail(format!("文件索引失败：{error:#}")),
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
                Err(error) => self.ui.fail(format!("修改检查失败：{error:#}")),
            }
        }
        if let Some(result) = finished_task(&mut self.export).await {
            match result {
                Ok(path) => {
                    self.ui.notify("报告已导出 · Ctrl+P 查看导出报告");
                    self.exported_report = Some(path);
                }
                Err(error) => self.ui.fail(format!("导出失败：{error:#}")),
            }
        }
    }
    fn submit_command(&mut self, engine: &mut Engine, data: &Path, text: &str) -> Result<bool> {
        let command = text.split_whitespace().next().unwrap_or("");
        ensure!(
            COMMANDS.iter().any(|(public, _)| *public == command),
            "未知命令 {command}；Ctrl+P 或 /help 查看命令（未发送给模型）"
        );
        let draft = self.ui.draft_snapshot();
        self.ui.close_completion();
        self.ui.take_draft();
        let result = self.command(engine, data, text);
        if result.is_err() {
            self.ui.restore_draft(draft);
        }
        result
    }

    fn command(&mut self, engine: &mut Engine, data: &Path, text: &str) -> Result<bool> {
        let (command, args) = text
            .trim()
            .split_once(char::is_whitespace)
            .unwrap_or((text.trim(), ""));
        let args = args.trim();
        match command {
            "/quit" => return Ok(true),
            "/help" => self.ui.open_help(),
            "/new" => {
                self.switch_session(engine, data, None)?;
            }
            "/model" => {
                if args.is_empty() {
                    self.model_form(data)?;
                } else {
                    self.set_model(engine, data, args)?;
                }
            }
            "/connect" => {
                if args.is_empty() {
                    self.connections(data)?;
                } else {
                    self.connection_choice(engine, data, args)?;
                }
            }
            "/questions" => self.questions(engine),
            "/message" => self.cancel_reply(),
            "/diff" => {
                self.start_diff(engine);
            }
            "/export" => {
                if args == "show" {
                    let path = self.exported_report.as_ref().context("本次尚未导出报告")?;
                    self.detail_event = None;
                    self.ui
                        .open_detail("导出报告 · Ctrl+Y 复制路径", path.display().to_string());
                } else {
                    ensure!(
                        args.is_empty(),
                        "使用 /export 导出，或 /export show 查看已导出的路径"
                    );
                    ensure!(self.export.is_none(), "export is already running");
                    let data = data.to_owned();
                    let id = engine.state().id.clone();
                    self.export = Some(tokio::task::spawn_blocking(move || {
                        services::export(&data, &id)
                    }));
                    self.ui.notify("正在导出对话和行动记录…");
                }
            }
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
        #[cfg(windows)]
        let mut command = {
            let mut command = tokio::process::Command::new("cmd.exe");
            // EDITOR is trusted local command text; the generated draft path is
            // expanded once inside quotes rather than interpolated as code.
            command
                .args(["/D", "/S", "/C"])
                .raw_arg(format!("{editor} \"%BONE_EDITOR_PATH%\""))
                .env("BONE_EDITOR_PATH", &path);
            command
        };
        command.kill_on_drop(true);
        let result = async {
            let mut child = command.spawn().context("start external editor")?;
            #[cfg(unix)]
            let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
            #[cfg(not(unix))] let mut term = ();
            let mut refresh = tokio::time::interval(Duration::from_millis(50));
            loop {
                self.maintain_execution(engine)?;
                tokio::select! {
                    result = child.wait() => break result.context("wait for external editor"),
                    _ = termination(&mut term) => { self.exit_requested = true; child.start_kill()?; bail!("external editor interrupted by SIGTERM; original draft retained") },
                    _ = tokio::signal::ctrl_c() => { engine.stop()?; child.start_kill()?; bail!("external editor interrupted; original draft retained") },
                    _ = refresh.tick() => {},
                    (id, result) = poll_engines(engine, &mut self.background) => {
                        self.execution_update(engine, data, &id, result)?;
                    },
                }
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
        let text = edited?;
        self.ui.take_draft();
        self.ui.paste(&text);
        self.ui
            .notify("已返回编辑后的草稿，Enter 才发送；执行保持继续");
        self.save(engine, data)?;
        Ok(())
    }
}

/// Dropping this wait cancels only polling, never Engine-owned execution tasks.
async fn poll_engines(
    current: &mut Engine,
    background: &mut BTreeMap<String, Engine>,
) -> (String, Result<Vec<Event>>) {
    let mut waits: FuturesUnordered<_> = std::iter::once(current)
        .chain(background.values_mut())
        .filter(|engine| !engine.is_quiescent())
        .map(|engine| async move { (engine.state().id.clone(), engine.step().await) })
        .collect();
    if waits.is_empty() {
        std::future::pending().await
    } else {
        waits.next().await.unwrap()
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
fn model_name(profile: &Profile) -> String {
    model_label(profile)
        .split_once(':')
        .map(|(_, name)| name.to_owned())
        .unwrap_or_default()
}
fn model_provider(profile: &Profile) -> String {
    model_label(profile)
        .split_once(':')
        .map(|(provider, _)| provider.to_owned())
        .unwrap_or_default()
}
fn field(label: &str, value: String, secret: bool) -> FormField {
    FormField {
        label: label.into(),
        value,
        secret,
    }
}
fn action(label: &str, value: &str, detail: &str) -> PickerItem {
    PickerItem {
        label: label.into(),
        value: value.into(),
        detail: detail.into(),
    }
}
fn unused_connection_name(config: &Config, data: &Path, base: &str) -> String {
    for index in 0.. {
        let name = if index == 0 {
            base.into()
        } else {
            format!("{base}_{index}")
        };
        if !config.profiles.contains_key(&name) && !data.join("profiles").join(&name).exists() {
            return name;
        }
    }
    unreachable!()
}
fn cleanup_connection(data: &Path, name: &str) {
    // Only fresh, reserved directories use this cleanup. An unreadable config
    // leaves credentials in place; uncertainty must not remove a live source.
    if Config::load(data)
        .ok()
        .is_some_and(|config| !config.profiles.contains_key(name))
    {
        let _ = std::fs::remove_dir_all(data.join("profiles").join(name));
    }
}
const COMMANDS: &[(&str, &str)] = &[
    ("/new", "新对话，当前工作继续执行"),
    ("/model", "修改当前连接的模型"),
    ("/connect", "选择连接，登录 ChatGPT 或添加 API"),
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
fn session_items(data: &Path, workspace: &Path, current: &str) -> Result<Vec<SessionItem>> {
    // Reuse one read-only connection for the session-list projection.
    let connection = rusqlite::Connection::open_with_flags(
        data.join("sessions.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    connection.busy_timeout(Duration::from_secs(5))?;
    let mut catalog = connection.prepare(
        "SELECT snapshot,
           (SELECT strftime(
              CASE WHEN strftime('%Y',
                json_extract(COALESCE(metadata, payload), '$.timestamp') / 1000.0,
                'unixepoch', 'localtime') = strftime('%Y', 'now', 'localtime')
              THEN '%m/%d %H:%M' ELSE '%Y/%m/%d' END,
              json_extract(COALESCE(metadata, payload), '$.timestamp') / 1000.0,
              'unixepoch', 'localtime')
            FROM events WHERE session_id = sessions.id ORDER BY sequence DESC LIMIT 1)
         FROM sessions
         WHERE json_extract(snapshot, '$.workspace') = ?1
           AND (EXISTS (SELECT 1 FROM events WHERE session_id = sessions.id
                  AND json_extract(payload, '$.kind') = 'input'
                  AND json_extract(payload, '$.data.source') = 'user')
             OR EXISTS (SELECT 1 FROM json_each(snapshot, '$.jobs'))
             OR json_array_length(snapshot, '$.pending_inputs') > 0)
         ORDER BY (id = ?2) DESC,
           COALESCE((SELECT MAX(sequence) FROM events WHERE session_id = sessions.id), 0) DESC,
           rowid DESC LIMIT 200",
    )?;
    let snapshots = catalog.query_map(
        rusqlite::params![
            workspace.to_str().context("workspace path is not UTF-8")?,
            current
        ],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
    )?;
    let mut inputs = connection.prepare(
        "SELECT payload FROM events WHERE session_id = ?1
         AND json_extract(payload, '$.kind') = 'input'
         AND json_extract(payload, '$.data.source') = 'user' ORDER BY sequence",
    )?;
    let mut facts = connection.prepare(
        "SELECT COALESCE(metadata, payload) FROM events WHERE session_id = ?1
         AND json_extract(COALESCE(metadata, payload), '$.kind') IN ('question','tool_result')
         ORDER BY sequence",
    )?;
    let mut items = Vec::new();
    for snapshot in snapshots {
        let (snapshot, updated_at) = snapshot?;
        let session: bone::state::SessionState = serde_json::from_str(&snapshot)?;
        let mut title = None;
        for payload in inputs.query_map([&session.id], |row| row.get::<_, String>(0))? {
            let event: Event = serde_json::from_str(&payload?)?;
            let text = native_text(&event.data["message"]);
            if let Some(line) = text.lines().map(str::trim).find(|line| !line.is_empty()) {
                // Keep both ends of exceptionally long input, without model work.
                let chars: Vec<char> = line.chars().collect();
                title = Some(if chars.len() > 160 {
                    chars[..120].iter().collect::<String>()
                        + "…"
                        + &chars[chars.len() - 39..].iter().collect::<String>()
                } else {
                    line.to_owned()
                });
                break;
            }
        }
        let events = facts
            .query_map([&session.id], |row| row.get::<_, String>(0))?
            .map(|row| Ok(serde_json::from_str::<Event>(&row?)?))
            .collect::<Result<Vec<_>>>()?;
        items.push(SessionItem {
            label: title.unwrap_or_else(|| "会话".into()),
            detail: session_status(&session, &events).into(),
            value: session.id,
            updated_at,
        });
    }
    Ok(items)
}
fn session_status(session: &bone::state::SessionState, events: &[Event]) -> &'static str {
    let answered = events
        .iter()
        .filter(|event| event.kind == "tool_result")
        .filter_map(|event| event.data["tool_key"].as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let has_question = events.iter().any(|question| {
        question.kind == "question"
            && question
                .job_id
                .as_ref()
                .and_then(|id| session.jobs.get(id))
                .is_some_and(|job| job.state != JobState::Closed)
            && question.data["tool_key"]
                .as_str()
                .is_some_and(|key| !answered.contains(key))
    });
    if has_question {
        "回复"
    } else if session.paused && has_unfinished_work(session) {
        "暂停"
    } else if !session.pending_inputs.is_empty()
        || session
            .jobs
            .values()
            .any(|job| matches!(job.state, JobState::Ready | JobState::Running))
    {
        "运行"
    } else if session
        .jobs
        .values()
        .any(|job| job.state == JobState::Waiting)
    {
        "等待"
    } else {
        ""
    }
}
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}
async fn copy_to_clipboard(text: &str) -> Result<()> {
    use std::process::Stdio;
    use tokio::io::AsyncWriteExt;
    #[cfg(windows)]
    let input: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
    #[cfg(windows)]
    let input = input.as_slice();
    #[cfg(not(windows))]
    let input = text.as_bytes();
    let choices: &[(&str, &[&str])] = if cfg!(target_os = "macos") {
        &[("pbcopy", &[])]
    } else if cfg!(windows) {
        &[("clip.exe", &[])]
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
            stdin.write_all(input).await?;
            drop(stdin);
            ensure!(
                child.wait().await?.success(),
                "系统剪贴板不可用；Ctrl+P 可导出对话记录"
            );
            return Ok(());
        }
        bail!("系统剪贴板不可用；Ctrl+P 可导出对话记录")
    })
    .await
    .context("clipboard command timed out")?
}
fn pause(engine: &mut Engine, ui: &mut View) -> Result<()> {
    if !engine.state().paused {
        engine.stop()?;
    }
    ui.notify(if engine.is_quiescent() {
        "工作已停止；Ctrl+R 继续，Ctrl+Q 退出"
    } else {
        "停止中；正在终止执行"
    });
    Ok(())
}
fn resume(engine: &mut Engine, ui: &mut View) {
    match engine.resume() {
        Ok(()) => ui.notify("已恢复；可以继续补充要求"),
        Err(error) => ui.fail(format!("无法恢复：{error:#}")),
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
    let stopped = stop_all(&mut terminal, engine, data, &mut app).await;
    abort_task(&mut app.diff);
    abort_task(&mut app.file_index);
    abort_task(&mut app.session_index);
    abort_task(&mut app.search);
    abort_task(&mut app.export);
    app.cancel_login();
    drop(terminal);
    eprintln!(
        "Session: {}\n继续：bone --data-dir {} --profile {} --model {} tui --session {} --workspace {}{}\n{}",
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
        },
        if has_unfinished_work(engine.state()) {
            "打开后 Ctrl+R 恢复暂停的工作。"
        } else {
            "打开后可继续提出要求。"
        },
    );
    result?;
    saved?;
    stopped
}

async fn stop_all(
    terminal: &mut Terminal,
    current: &mut Engine,
    data: &Path,
    app: &mut App,
) -> Result<()> {
    let mut failed = BTreeSet::new();
    let mut first_error = None;
    for engine in std::iter::once(&mut *current).chain(app.background.values_mut()) {
        if !engine.state().paused
            && (!engine.is_quiescent() || has_unfinished_work(engine.state()))
            && let Err(error) = engine.stop()
        {
            failed.insert(engine.state().id.clone());
            first_error.get_or_insert(error);
        }
    }
    loop {
        app.metadata(current);
        let model = format!(
            "{} · {}",
            app.settings.profile_name,
            model_name(&app.settings.profile)
        );
        let fact = app.ui.live_status.clone();
        terminal
            .terminal
            .draw(|frame| app.ui.render(frame, current.state(), &model, &fact))?;
        let next = {
            let mut waits: FuturesUnordered<_> = std::iter::once(&mut *current)
                .chain(app.background.values_mut())
                .filter(|engine| !engine.is_quiescent() && !failed.contains(&engine.state().id))
                .map(|engine| async move { (engine.state().id.clone(), engine.step().await) })
                .collect();
            waits.next().await
        };
        let Some((id, result)) = next else {
            break;
        };
        if let Err(error) = result {
            failed.insert(id.clone());
            app.ui.fail(format!(
                "会话 {} 停止记录失败：{error:#}；重新打开可恢复持久记录",
                short_id(&id)
            ));
            first_error.get_or_insert(error);
        } else if let Err(error) = app.execution_update(current, data, &id, result) {
            first_error.get_or_insert(error);
        }
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
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
    app.duration = duration;
    let mut cooldown = Instant::now();
    let mut dirty = true;
    let mut tick = tokio::time::interval(Duration::from_millis(50));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    #[cfg(unix)]
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    #[cfg(not(unix))]
    let mut terminate = ();
    loop {
        if app.exit_requested {
            return Ok(());
        }
        if dirty {
            app.metadata(engine);
            let model = format!(
                "{} · {}",
                app.settings.profile_name,
                model_name(&app.settings.profile)
            );
            let fact = app.ui.live_status.clone();
            terminal
                .terminal
                .draw(|frame| app.ui.render(frame, engine.state(), &model, &fact))?;
            dirty = false;
        }
        app.maintain_execution(engine)?;
        tokio::select! {
            biased;
            event = next_terminal(&mut input) => {
                let revision = engine.state().revision;
                match event? {
                    TerminalEvent::Key(key) if key.kind != KeyEventKind::Release => {
                        if app.ui.notice_tone == Tone::Error && !app.ui.has_modal() && app.ui.focus == Focus::Input
                            && (key.code == KeyCode::Esc || key.code == KeyCode::Backspace || key.code == KeyCode::Delete
                                || matches!(key.code, KeyCode::Char(_)) && !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)) {
                            app.ui.notify("");
                        }
                        let control = key.modifiers.contains(KeyModifiers::CONTROL);
                        match key.code {
                            KeyCode::Char('q') if control => return Ok(()),
                            KeyCode::Char('c') if control => {
                                pause(engine, &mut app.ui)?; app.clear_preview(); app.deadlines.remove(&engine.state().id);
                                if app.login.is_some() { app.cancel_login(); app.ui.close_layer(); app.ui.notify("登录已取消，当前连接保留"); }
                            },
                            KeyCode::Char('y') if control => { if let Err(error) = app.copy_selected(engine).await { app.ui.fail(format!("复制失败：{error:#}")); } },
                            KeyCode::Esc if app.login.is_some() => { app.cancel_login(); app.ui.close_layer(); app.ui.notify("登录已取消，当前连接和草稿保留"); },
                            KeyCode::F(1) if app.login.is_some() => {},
                            _ if app.login.is_some() => app.ui.handle_key(key),
                            KeyCode::Esc if app.ui.form_is_open() => { app.ui.close_layer(); app.connection_setup = None; },
                            KeyCode::Enter if app.ui.form_is_open() => {
                                if let Err(error) = app.submit_connection_form(engine, data) { app.ui.form_error(format!("{error:#}")); }
                            },
                            _ if app.ui.form_is_open() => app.ui.handle_key(key),
                            KeyCode::Char('f') if control => app.start_search(engine, data, ""),
                            KeyCode::Char('d') if control => app.start_diff(engine),
                            KeyCode::Char('D') if !control && (app.ui.detail.is_some() || (app.ui.focus == Focus::Conversation && !app.ui.has_modal())) => {
                                if let Err(error) = app.open_audit(engine) { app.ui.fail(format!("读取审计失败：{error:#}")); }
                            },
                            KeyCode::Char('d') if app.ui.focus == Focus::Conversation && !app.ui.has_modal() => {
                                if let Err(error) = app.open_selected(engine) { app.ui.fail(format!("读取失败：{error:#}")); }
                            },
                            KeyCode::Char('y') if app.ui.focus == Focus::Conversation && !app.ui.has_modal() => {
                                if let Err(error) = app.copy_selected(engine).await { app.ui.fail(format!("复制失败：{error:#}")); }
                            },
                            KeyCode::Char('r') if control => { resume(engine, &mut app.ui); app.deadlines.insert(engine.state().id.clone(), Instant::now()+duration); },
                            KeyCode::End if app.ui.focus == Focus::Conversation && !app.ui.has_modal() => {
                                if let Err(error) = app.refresh_latest(engine,data) { app.ui.fail(format!("读取最新记录失败：{error:#}")); }
                                app.ui.handle_key(key);
                                if let Some(id)=app.ui.messages.last().and_then(|message|message.event_id.clone()) { app.ui.select_message(&id); }
                            },
                            KeyCode::Char('p') if control => app.commands(engine, String::new()),
                            KeyCode::Char('o') if control => app.complete_file(engine),
                            KeyCode::Char('g') if control => {
                                if let Err(error) = app.editor(engine, data, terminal, &mut input).await { app.ui.fail(format!("编辑失败：{error:#}")); }
                            },
                            KeyCode::Tab | KeyCode::Enter if app.ui.is_completion() && app.ui.picker_value().is_some()
                                && !key.modifiers.intersects(KeyModifiers::ALT | KeyModifiers::CONTROL | KeyModifiers::SHIFT) => {
                                if let Some((kind, value)) = app.ui.picker_value() {
                                    if kind == PickerKind::Command && key.code == KeyCode::Enter {
                                        match app.submit_command(engine, data, &value) {
                                            Ok(true) => return Ok(()),
                                            Ok(false) => {},
                                            Err(error) => app.ui.fail(format!("操作失败：{error:#}；草稿已保留")),
                                        }
                                    } else { app.complete_inline(kind, &value); }
                                }
                            },
                            KeyCode::Enter if app.ui.picker.is_some() && !app.ui.is_completion() => {
                                if let Some((kind, value)) = app.ui.picker_value() {
                                    app.ui.close_layer();
                                    let result = match kind {
                                        PickerKind::Command => app.command(engine, data, &value),
                                        PickerKind::Session => app.switch_session(engine, data, Some(&value)).map(|_|false),
                                        PickerKind::Connection => app.connection_choice(engine, data, &value).map(|_|false),
                                        PickerKind::History => {
                                            app.ui.select_message(&value);
                                            engine.read_event(&value).and_then(|e| detail(engine, &e)).map(|text| {
                                                app.detail_event = Some(value.clone()); app.ui.open_detail(format!("搜索原文 · {} · {} · F2 审计", app.search_query, short_id(&value)), text); app.remember_detail_source(); false
                                            })
                                        },
                                        PickerKind::Question => app.bind_reply(engine, &value).map(|_| false),
                                        PickerKind::File => {
                                            let reference = if value.contains(char::is_whitespace) { format!("@\"{value}\" ") } else { format!("@{value} ") };
                                            if let Some(range) = app.file_range.take() { app.ui.replace_range(range, &reference); }
                                            else { app.ui.paste(&reference); }
                                            Ok(false)
                                        },
                                    };
                                    match result { Ok(true) => return Ok(()), Ok(false) => {}, Err(error) => app.ui.fail(format!("操作失败：{error:#}"))}
                                }
                            },
                            _ if app.ui.has_modal() => app.ui.handle_key(key),
                            KeyCode::Enter if !key.modifiers.intersects(KeyModifiers::ALT | KeyModifiers::CONTROL | KeyModifiers::SHIFT) => {
                                match app.ui.focus {
                                    Focus::Input => {
                                        let text = app.ui.draft().clone();
                                        if !text.trim().is_empty() {
                                            app.ui.close_completion();
                                            if text.trim_start().starts_with('/') {
                                                // Keep an unknown command in the editor; do not turn it into agent input.
                                                let result = app.submit_command(engine, data, &text);
                                                match result {
                                                    Ok(true) => return Ok(()),
                                                    Ok(false) => {},
                                                    Err(error) => app.ui.fail(format!("操作失败：{error:#}；草稿已保留")),
                                                }
                                            } else {
                                                let posted = if let Some(target) = app.reply_target.as_deref() { engine.post(&text, Some(target)) } else { engine.post_message(&text) };
                                                match posted {
                                                    Ok(id) => { app.ui.remember_prompt(&text); app.ui.take_draft(); app.clear_preview();
                                                        let sent_target = app.reply_target.clone();
                                                        app.drafts.remove(&sent_target.clone().unwrap_or_default());
                                                        if sent_target.is_some() { app.switch_target(None); }
                                                        app.last_input = Some(id.clone());
                                                        app.last_admitted_input = None;
                                                        app.ui.notify("已收到 · 等待纳入执行");
                                                        app.elapsed_from = Instant::now(); app.deadlines.insert(engine.state().id.clone(), Instant::now()+duration);
                                                        app.refresh_sessions(engine, data); },
                                                    Err(error) => app.ui.fail(format!("发送失败：{error:#}；草稿已保留")),
                                                }
                                            }
                                        }
                                    },
                                    Focus::Activity => {
                                        if let Some(id) = app.ui.selected_event().map(str::to_owned) {
                                            match engine.read_event(&id).and_then(|e| detail(engine, &e)) {
                                                Ok(text) => { app.detail_event = Some(id.clone()); app.ui.open_detail(format!("行动原文 · {} · F2 审计", short_id(&id)), text); app.remember_detail_source(); },
                                                Err(error) => app.ui.fail(format!("读取记录失败：{error:#}")),
                                            }
                                        }
                                    },
                                    Focus::Conversation => {
                                        let question = app.ui.selected_message().and_then(|m| m.event_id.as_deref())
                                            .filter(|id| engine.unanswered_questions().iter().any(|q| q.id == *id)).map(str::to_owned);
                                        if let Some(id) = question {
                                            if let Err(error) = app.bind_reply(engine, &id) { app.ui.fail(format!("回复目标未切换：{error:#}")); }
                                        } else { app.ui.toggle_selected_message(); }
                                    },
                                    Focus::Sessions => {
                                        if let Some(id) = app.ui.selected_session().map(str::to_owned)
                                            && let Err(error) = app.switch_session(engine, data, Some(&id)) {
                                            app.ui.fail(format!("会话未切换：{error:#}"));
                                        }
                                    },
                                }
                            },
                            _ => app.ui.handle_key(key),
                        }
                        if key.modifiers.is_empty()
                            && matches!(key.code, KeyCode::Up | KeyCode::Home | KeyCode::PageUp | KeyCode::Char('k'))
                            && app.ui.focus == Focus::Conversation
                            && app.ui.at_history_start() && app.older.is_some()
                            && let Err(error) = app.load_older(engine, data) {
                            app.ui.fail(format!("读取更早记录失败：{error:#}"));
                        }
                        if key.code == KeyCode::Left && key.modifiers == KeyModifiers::SHIFT
                            && app.ui.focus == Focus::Sessions && !app.ui.has_modal() && !app.ui.show_activity {
                            app.refresh_sessions(engine, data);
                        }
                        app.refresh_search(engine, data);
                        if let Err(error) = app.refresh_completion(engine) { app.ui.fail(format!("补全失败：{error:#}")); }
                        app.changed_at = Some(Instant::now()); dirty = true;
                    },
                    TerminalEvent::Paste(text) => {
                        if app.ui.notice_tone == Tone::Error && !app.ui.has_modal() && app.ui.focus == Focus::Input { app.ui.notify(""); }
                        app.ui.handle_paste(&text); app.refresh_search(engine, data); app.refresh_completion(engine)?; app.changed_at = Some(Instant::now()); dirty = true;
                    },
                    TerminalEvent::Mouse(mouse) => {
                        if let Some(id) = app.ui.clicked_session(mouse) {
                            if let Err(error) = app.switch_session(engine, data, Some(&id)) {
                                app.ui.fail(format!("会话未切换：{error:#}"));
                            }
                        } else {
                            app.ui.handle_mouse(mouse);
                            if mouse.kind == crossterm::event::MouseEventKind::ScrollUp
                                && app.ui.conversation_area.contains((mouse.column, mouse.row).into())
                                && app.ui.at_history_top() && app.older.is_some()
                                && let Err(error) = app.load_older(engine, data) {
                                app.ui.fail(format!("读取更早记录失败：{error:#}"));
                            }
                        }
                        dirty = true;
                    },
                    TerminalEvent::Resize(..) => dirty = true,
                    _ => {},
                }
                if engine.state().revision != revision {
                    app.clear_preview(); app.sync(engine, data)?;

                }
            },
            _ = termination(&mut terminate) => return Ok(()),
            _ = tokio::signal::ctrl_c() => { pause(engine,&mut app.ui)?; app.clear_preview(); app.deadlines.remove(&engine.state().id); app.sync(engine,data)?; dirty = true; },
            _ = tick.tick(), if !engine.is_quiescent() || !app.background.is_empty() || app.has_tasks() || app.changed_at.is_some() || app.ui.notice_until.is_some() => {
                let pending = app.has_tasks();
                app.tasks(engine, data).await;
                let expired_notice = app.ui.expire_notice();
                dirty |= app.progress(engine) || !engine.is_quiescent() || pending || expired_notice;
                if app.changed_at.is_some_and(|at| at.elapsed() >= Duration::from_millis(500))
                    && let Err(error) = app.save(engine,data) {
                    app.ui.fail(format!("草稿保存失败：{error:#}")); app.changed_at = None; dirty = true;
                }
            },
            _ = tokio::time::sleep_until(cooldown), if Instant::now() < cooldown => {},
            (id, result) = poll_engines(engine, &mut app.background), if Instant::now() >= cooldown => {
                if result.as_ref().is_ok_and(Vec::is_empty) { cooldown = Instant::now()+Duration::from_millis(25); }
                app.execution_update(engine, data, &id, result)?;
                dirty = true;
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
}

fn execution_active(state: &bone::state::SessionState) -> bool {
    !state.paused
        && state.jobs.values().any(|job| {
            job.state == JobState::Ready
                || (job.state == JobState::Running && job.current_call.is_some())
        })
}

fn has_unfinished_work(state: &bone::state::SessionState) -> bool {
    !state.pending_inputs.is_empty()
        || state.jobs.values().any(|job| {
            job.state != JobState::Closed
                && (job.state != JobState::Idle
                    || job.current_call.is_some()
                    || job.active_input.is_some()
                    || !job.inbox.is_empty()
                    || !job.wait_for.is_empty())
        })
}

fn active_call(engine: &Engine, call: &str) -> Result<ActiveCall> {
    let event = engine
        .read_call_event(call, "tool_started")
        .or_else(|_| engine.read_call_event(call, "model_started"))?;
    let phase = if event.kind == "model_started" {
        if !event.data["recovery_of"].is_null() {
            "本轮生成未完成，正在继续"
        } else if event.data["purpose"] == "summary" {
            "整理上下文"
        } else {
            "思考中"
        }
    } else {
        match event.data["tool_name"].as_str().unwrap_or("") {
            "shell" => "执行命令",
            "read_file" => "读取文件",
            "search_files" => "搜索文件",
            "list_files" => "查看文件",
            "write_file" => "写入文件",
            "edit_file" => "修改文件",
            _ => "执行工具",
        }
    };
    let target = if event.kind == "tool_started" {
        tool_arguments(engine, &event)?
            .and_then(|args| {
                args["path"]
                    .as_str()
                    .or_else(|| args["command"].as_str())
                    .or_else(|| args["query"].as_str())
                    .map(|text| question_summary(text, 24))
            })
            .unwrap_or_default()
    } else {
        String::new()
    };
    let age_ms = event
        .timestamp
        .parse::<u64>()
        .ok()
        .and_then(|stamp| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .map(|now| (now.as_millis().min(u64::MAX as u128) as u64).saturating_sub(stamp))
        })
        .unwrap_or(0);
    let started = Instant::now()
        .checked_sub(Duration::from_millis(age_ms))
        .unwrap_or_else(Instant::now);
    Ok(ActiveCall {
        phase,
        target,
        started,
    })
}

fn generation_failure_text(error: &str) -> Option<String> {
    let reason = if error.starts_with("generation remained incomplete") {
        "较小动作的续做仍未完成，已暂停"
    } else if error.starts_with("summary generation remained incomplete") {
        "较小历史片段的摘要仍未完成，已暂停"
    } else if error.starts_with("summary reached a generation limit") {
        "摘要达到限制，无法选取更小的完整历史片段"
    } else if error.starts_with("model context limit reached") {
        "模型上下文达到限制"
    } else if error.starts_with("provider filtered the generation") {
        "服务端过滤了本轮生成"
    } else if error.starts_with("provider ended generation with") {
        "服务端报告了未识别的结束原因"
    } else if error.starts_with("summary did not reduce") {
        "摘要没有缩小后续请求"
    } else if error.starts_with("summary returned no text")
        || error.starts_with("summary unexpectedly requested")
    {
        "摘要未返回有效的完整文本"
    } else {
        return None;
    };
    Some(format!("{reason}。已完成的操作和原始历史已保留。\n{error}"))
}

fn current_status(engine: &Engine, input: Option<&str>, tool_failure: Option<&str>) -> String {
    let state = engine.state();

    if state.paused && !engine.is_quiescent() {
        return "停止中 · 等待执行结束".into();
    }
    if state.paused && has_unfinished_work(state) {
        return "已停止 · Ctrl+R 继续".into();
    }
    let active = state
        .jobs
        .values()
        .filter(|job| job.state == JobState::Running && job.current_call.is_some())
        .count();
    if active > 0 {
        if state
            .jobs
            .values()
            .filter_map(|job| job.current_call.as_ref())
            .any(|call| {
                engine
                    .read_call_event(call, "model_started")
                    .is_ok_and(|e| !e.data["recovery_of"].is_null())
            })
        {
            return "本轮生成未完成，正在继续".into();
        }
        return if active == 1 {
            "正在执行".into()
        } else {
            format!("正在执行 · {active} 项并行")
        };
    }
    if state.jobs.values().any(|job| job.state == JobState::Ready) {
        return "处理中".into();
    }
    let terminal = input.and_then(|id| engine.result(id)).filter(|event| {
        matches!(
            event.kind.as_str(),
            "delivery" | "failure" | "input_paused" | "input_resolved"
        )
    });
    if terminal.is_some_and(|event| event.kind != "delivery") {
        if terminal.is_some_and(|event| {
            event.kind == "failure"
                && event.data["error"]
                    .as_str()
                    .is_some_and(|e| generation_failure_text(e).is_some())
        }) {
            return "生成暂停 · 已有进度保留".into();
        }
        return terminal_label(&terminal.unwrap().kind).into();
    }
    if !state.pending_inputs.is_empty() {
        return format!("{} 条新要求已接收 · 等待处理", state.pending_inputs.len());
    }
    let questions = engine.unanswered_questions().len();
    if questions > 0 {
        return format!("等待回复 · {questions} 个问题");
    }
    if state
        .jobs
        .values()
        .any(|job| job.state == JobState::Waiting)
    {
        return "等待工作结果".into();
    }
    if terminal.is_some() {
        return "本次已完成".into();
    }
    if let Some(failure) = tool_failure {
        return format!("最近工具失败 {failure} · 暂无活动调用");
    }
    "等待输入".into()
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
        "{}{result}",
        target.map(|t| format!("{t} · ")).unwrap_or_default(),
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
        output = "此记录没有正文；F2 查看持久事件与审计元信息".into();
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
                "执行中断，实际效果未确认"
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
            "{}\n```text\n{}\n```",
            if args["mode"] == "append" {
                "拟追加到文件末尾"
            } else {
                "拟写入内容（可能覆盖现有文件）"
            },
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
        let kind = if user {
            MessageKind::User { admitted: false }
        } else if event.kind == "question" {
            MessageKind::Question
        } else if event.kind == "failure" {
            MessageKind::Failure
        } else if event.kind == "input_paused" {
            MessageKind::Paused
        } else {
            MessageKind::Delivery
        };
        ui.push_message(Message {
            summary: None,
            kind,
            text: if event.kind == "failure" {
                generation_failure_text(&body).unwrap_or_else(|| body.clone())
            } else {
                body.clone()
            },
            event_id: Some(event.id.clone()),
        });
    }
    if event.kind == "input_handled"
        && let Some(id) = event.data["input_id"].as_str()
    {
        let input = engine.read_event(id)?;
        if input.data["source"] == "user" {
            ui.upsert_message(Message {
                kind: MessageKind::User { admitted: true },
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
        let state = if event.kind == "tool_started" {
            ToolState::Running
        } else if event.data["uncertain"] == true {
            ToolState::Unknown
        } else if failed {
            ToolState::Failed
        } else {
            ToolState::Completed
        };
        let phase = match state {
            ToolState::Running => "执行中",
            ToolState::Unknown => "执行中断 · 结果未确认",
            ToolState::Failed => "失败",
            ToolState::Completed => "完成",
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
            kind: MessageKind::Tool {
                name: name.into(),
                state,
            },
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
                kind: MessageKind::Progress,
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
        "model_limited" => (
            if event.data["retry_allowed"] == true {
                "本轮生成未完成，正在继续".into()
            } else {
                "生成仍未完成 · 已保留进度".into()
            },
            Tone::Info,
        ),
        "question" => ("向你提问".into(), Tone::Info),
        "delivery" => ("本轮交付".into(), Tone::Success),
        "failure" | "model_failed" => ("执行失败".into(), Tone::Error),
        "model_cancelled" | "cancelled" => ("调用已取消".into(), Tone::Normal),
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
