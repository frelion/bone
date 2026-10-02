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
use view::{Activity, Focus, Message, PickerItem, PickerKind, Tone, View};

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
        }
    }
    fn load(&mut self, engine: &mut Engine, data: &Path) -> Result<()> {
        self.ui = View::new();
        self.parts.clear();
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
            "已加载最近记录；/older 浏览更早原文 · Ctrl+P 命令"
        } else {
            "Enter 发送 · Shift+Enter 换行 · Ctrl+P 命令 · @文件 Tab 补全"
        }
        .into();
        match services::load(data, &engine.state().id) {
            Ok(saved) => {
                if !saved.history.is_empty() {
                    self.ui.set_history(saved.history.clone());
                }
                self.ui.paste(&saved.draft);
                self.last_saved = saved;
                if !self.ui.draft.is_empty() {
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
        self.refresh_usage(engine);
        self.metadata(engine);
        Ok(())
    }
    fn save(&mut self, engine: &Engine, data: &Path) -> Result<()> {
        let saved = services::UiSaved {
            draft: self.ui.draft.clone(),
            history: self.ui.history(),
        };
        if saved.draft != self.last_saved.draft || saved.history != self.last_saved.history {
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
    }
    fn ingest(&mut self, engine: &Engine, event: &Event) -> Result<()> {
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
        ingest(engine, event, &mut self.ui)
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
        self.ui.busy = !engine.is_quiescent();
        self.ui.session_label = engine.state().id.chars().take(8).collect();
        self.ui.live_status = if self.ui.busy {
            format!(
                "{} · {}s",
                status(engine),
                self.elapsed_from.elapsed().as_secs()
            )
        } else {
            status(engine)
        };
    }
    fn refresh_usage(&mut self, engine: &Engine) {
        self.ui.usage = if let Some(input) = &self.last_input {
            let m = engine.metrics(input);
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
                role: "Agent · 输出中（未交付）".into(),
                text,
                event_id: Some(format!("live:{}", progress.call_id)),
            });
            changed = true;
        }
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
        let prefix = &self.ui.draft[..cursor];
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
    fn has_tasks(&self) -> bool {
        self.file_index.is_some()
            || self.session_index.is_some()
            || self.diff.is_some()
            || self.export.is_some()
    }
    async fn tasks(&mut self) {
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
                    self.ui.detail = Some((
                        "HTML 导出完成".into(),
                        format!(
                            "{}\n\n报告包含持久化对话、工具行动和归属信息。可以用浏览器打开这个本地文件。",
                            path.display()
                        ),
                    ));
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
            "/help" => self.ui.show_help = true,
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
            "/search" => self.ui.start_search(args),
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
                    status(engine),
                    serde_json::to_string_pretty(engine.options())?,
                    self.last_input
                        .as_ref()
                        .map(|id| engine.metrics(id))
                        .unwrap_or(Value::Null),
                    serde_json::to_string_pretty(&state.unknown_writes)?
                );
                self.ui.detail = Some(("会话状态".into(), detail));
            }
            "/diff" => {
                abort_task(&mut self.diff);
                let workspace = engine.state().workspace.clone();
                self.diff = Some(tokio::spawn(
                    async move { services::git_diff(&workspace).await },
                ));
                self.ui.detail = Some(("项目修改（只读）".into(), "正在读取 Git 修改…".into()));
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
            "/copy" => {
                let event = self
                    .ui
                    .messages
                    .iter()
                    .rev()
                    .find(|m| m.role == "Agent")
                    .and_then(|m| m.event_id.as_deref())
                    .context("还没有可复制的正式答复")?;
                let text = engine.event_text(&engine.read_event(event)?)?;
                copy_to_clipboard(&text).await?;
                self.ui.notice = "已复制完整答复".into();
            }
            "/older" => {
                let page =
                    bone::history_before(data, &engine.state().id, self.older.as_deref(), 20)?;
                if page.events.is_empty() {
                    self.ui.notice = "已经是最早的记录".into();
                } else {
                    let mut text = String::new();
                    for event in &page.events {
                        let body = detail(engine, event)?;
                        text.push_str(&format!(
                            "## {}\n{}\n\n",
                            event.kind,
                            body.chars().take(16_000).collect::<String>()
                        ));
                    }
                    self.older = page.next_cursor;
                    self.ui.detail = Some(("更早会话原文 · /older 继续向前".into(), text));
                }
            }
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
            file.write_all(self.ui.draft.as_bytes())?;
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
    ("/search", "搜索当前对话；Ctrl+F"),
    ("/older", "分页读取更早会话原文"),
    ("/export", "导出本地 HTML 对话与行动报告"),
    ("/editor", "用 VISUAL / EDITOR 编辑长输入"),
    ("/copy", "复制最后一条完整答复到系统剪贴板"),
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
    drop(terminal);
    eprintln!("Session: {}", engine.state().id);
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
            terminal.terminal.draw(|frame| {
                app.ui
                    .render(frame, engine.state(), &model, &status(engine))
            })?;
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
                            KeyCode::Char('c') if control => { pause(engine, &mut app.ui)?; app.clear_preview(); deadline = None; },
                            KeyCode::Char('r') if control => { resume(engine, &mut app.ui); deadline = Some(Instant::now()+duration); },
                            KeyCode::Char('p') if control => app.commands(String::new()),
                            KeyCode::Char('o') if control => app.complete_file(engine),
                            KeyCode::Char('g') if control => {
                                if let Err(error) = app.editor(engine, data, terminal, &mut input).await { app.ui.notice = format!("编辑失败：{error:#}"); }
                            },
                            KeyCode::Tab if app.ui.focus == Focus::Input && !app.ui.has_modal()
                                && (app.ui.draft.starts_with('/') || app.ui.draft[..app.ui.cursor()].split_whitespace().last().is_some_and(|t| t.starts_with('@'))) => {
                                if app.ui.draft.starts_with('/') { app.commands(app.ui.draft.clone()); } else { app.complete_file(engine); }
                            },
                            KeyCode::Enter if app.ui.picker.is_some() => {
                                if let Some((kind, value)) = app.ui.picker_value() {
                                    app.ui.picker = None;
                                    let result = match kind {
                                        PickerKind::Command => {
                                            let previous = if app.ui.draft.trim_start().starts_with('/') { Some(app.ui.take_draft()) } else { None };
                                            let result = app.command(engine, data, terminal, &mut input, &value).await;
                                            if result.is_err() && let Some(previous) = previous { app.ui.paste(&previous); }
                                            result
                                        },
                                        PickerKind::Session => app.switch_session(engine, data, Some(&value)).map(|_|false),
                                        PickerKind::Model => app.set_model(engine, &value).map(|_|false),
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
                            KeyCode::Enter if !key.modifiers.intersects(KeyModifiers::ALT | KeyModifiers::CONTROL | KeyModifiers::SHIFT) => {
                                match app.ui.focus {
                                    Focus::Input => {
                                        let text = app.ui.draft.clone();
                                        if !text.trim().is_empty() {
                                            if text.trim_start().starts_with('/') {
                                                // Keep an unknown command in the editor; do not turn it into agent input.
                                                app.ui.take_draft();
                                                let result = app.command(engine, data, terminal, &mut input, &text).await;
                                                match result {
                                                    Ok(true) => return Ok(()),
                                                    Ok(false) => {},
                                                    Err(error) => { app.ui.paste(&text); app.ui.notice = format!("操作失败：{error:#}；草稿已保留"); },
                                                }
                                            } else {
                                                match engine.post(&text, None) {
                                                    Ok(_) => { app.ui.remember_prompt(&text); app.ui.take_draft(); app.clear_preview();
                                                        app.ui.notice = "已接收；运行中可继续补充要求".into();
                                                        app.elapsed_from = Instant::now(); deadline = Some(Instant::now()+duration); },
                                                    Err(error) => app.ui.notice = format!("发送失败：{error:#}；草稿已保留"),
                                                }
                                            }
                                        }
                                    },
                                    Focus::Activity => {
                                        if let Some(id) = app.ui.selected_event().map(str::to_owned) {
                                            match engine.read_event(&id).and_then(|e| detail(engine, &e)) {
                                                Ok(text) => app.ui.detail = Some(("行动原文（只读）".into(), text)),
                                                Err(error) => app.ui.notice = format!("读取记录失败：{error:#}"),
                                            }
                                        }
                                    },
                                    Focus::Conversation => {},
                                }
                            },
                            _ => app.ui.handle_key(key),
                        }
                        app.changed_at = Some(Instant::now()); dirty = true;
                    },
                    TerminalEvent::Paste(text) => { app.ui.handle_paste(&text); app.changed_at = Some(Instant::now()); dirty = true; },
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
            _ = tick.tick(), if !engine.is_quiescent() || app.has_tasks() || app.changed_at.is_some() => {
                let pending = app.has_tasks();
                app.tasks().await;
                dirty |= app.progress(engine) || !engine.is_quiescent() || pending;
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

fn status(engine: &Engine) -> String {
    let state = engine.state();
    if !state.unknown_writes.is_empty() {
        return format!(
            "需核查 {} 项写入 · 使用 bone reconcile",
            state.unknown_writes.len()
        );
    }
    if state.paused {
        return "已暂停 · Ctrl+R 恢复".into();
    }
    if !engine.unanswered_questions().is_empty() {
        return "等待你的回答 · 直接输入即可".into();
    }
    let running = state
        .jobs
        .values()
        .filter(|job| job.state == JobState::Running)
        .count();
    if running > 0 {
        return format!("工作中 · {running} 项正在执行");
    }
    if !engine.is_quiescent() {
        "正在安排工作".into()
    } else if state
        .jobs
        .values()
        .any(|job| job.state == JobState::Waiting)
    {
        "等待工作结果".into()
    } else {
        "就绪".into()
    }
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

fn detail(engine: &Engine, event: &Event) -> Result<String> {
    let metadata = format!(
        "event: {}\nkind: {}\njob: {}\ncall: {}\nreply_to: {}\nroot_input: {}\nrevision: {}\ntimestamp (Unix ms): {}\n",
        event.id,
        event.kind,
        event.job_id.as_deref().unwrap_or("—"),
        event.call_id.as_deref().unwrap_or("—"),
        event.reply_to.as_deref().unwrap_or("—"),
        event.root_input.as_deref().unwrap_or("—"),
        event.revision,
        event.timestamp
    );
    let text = event_body(engine, event)?;
    let mut output = metadata;
    if let Some(arguments) = tool_arguments(engine, event)? {
        let diff =
            proposed_change_preview(event.data["tool_name"].as_str().unwrap_or(""), &arguments);
        if !diff.is_empty() {
            output.push_str(&format!("\n修改预览（以工具执行结果为准）\n{diff}\n"));
        }
        output.push_str(&format!(
            "\n原生工具参数\n{}\n",
            serde_json::to_string_pretty(&arguments)?
        ));
    }
    if !text.is_empty() {
        output.push_str("\n原文\n");
        output.push_str(&text);
    } else {
        // Model metadata is useful; opaque provider reasoning/transport blocks
        // are not presented as a readable explanation of the Agent's thinking.
        let mut data = event.data.clone();
        if let Some(object) = data.as_object_mut() {
            object.remove("response");
            object.remove("stream_items");
        }
        output.push_str(&serde_json::to_string_pretty(&data)?);
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
                "\n原生工具提议\n{}",
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
            "你"
        } else if event.kind == "question" {
            "Agent · 提问"
        } else if event.kind == "failure" {
            "执行失败"
        } else {
            "Agent"
        };
        ui.push_message(Message {
            role: role.into(),
            text: body.clone(),
            event_id: Some(event.id.clone()),
        });
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
        let failed = event.data["error"].is_string()
            || result.starts_with("错误：")
            || result
                .lines()
                .next()
                .is_some_and(|line| line.starts_with("exit: ") && line != "exit: 0")
            || serde_json::from_str::<Value>(result)
                .ok()
                .is_some_and(|v| v.get("error").is_some());
        let phase = if event.kind == "tool_started" {
            "执行中"
        } else if failed {
            "失败"
        } else {
            "完成"
        };
        let text = format!(
            "{}{}{}\n{}",
            phase,
            if label.is_empty() { "" } else { " · " },
            label,
            result
                .lines()
                .take(4)
                .collect::<Vec<_>>()
                .join("\n")
                .chars()
                .take(700)
                .collect::<String>()
        );
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
