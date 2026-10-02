//! The single writer for one session. Model and tool futures return proposals;
//! only this loop can commit them or start the next external action.
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use fs2::FileExt;
use futures_util::StreamExt;
use rig_core::completion::{CompletionRequest, CompletionResponse, Message};
use rig_core::message::ToolCall;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::task::{AbortHandle, JoinSet};

use crate::config::Profile;
use crate::state::{Budget, Event, Job, JobState, SessionState, UnknownWrite, new_id};
use crate::store::{SessionLease, Store};
use crate::{context, model, tools};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunOptions {
    pub max_calls: u32,
    pub max_jobs: u32,
    pub max_parallel: usize,
    pub context_chars: usize,
    pub single_job: bool,
    pub no_compaction: bool,
    pub read_only: bool,
    pub model_timeout_seconds: u64,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            max_calls: 64,
            max_jobs: 16,
            max_parallel: 3,
            context_chars: 96_000,
            single_job: false,
            no_compaction: false,
            read_only: false,
            model_timeout_seconds: 180,
        }
    }
}

#[derive(Clone)]
struct Origin {
    job: String,
    input: String,
    root: String,
    call: String,
    revision: u64,
}

#[derive(Clone)]
struct PendingTool {
    call: ToolCall,
    key: String,
}

struct Running {
    origin: Origin,
    abort: AbortHandle,
    tool: Option<PendingTool>,
}

enum Completed {
    Model {
        origin: Origin,
        response: Result<CompletionResponse>,
        covered: Option<Vec<String>>,
        stream_items: Vec<Value>,
    },
    Tool {
        origin: Origin,
        tool: PendingTool,
        outcome: tools::ToolOutcome,
    },
}

/// Ephemeral observation of one Rig native stream item. Durable responses remain authoritative.
#[derive(Clone, Debug)]
pub struct ModelProgress {
    pub job_id: String,
    pub call_id: String,
    pub revision: u64,
    pub purpose: String,
    pub item: Value,
}

const MODEL_PROGRESS_RECORDS: usize = 128;
const MODEL_PROGRESS_ITEM_BYTES: usize = 32 * 1024;

#[derive(Default)]
struct ProgressObserver {
    enabled: AtomicBool,
    queue: Mutex<VecDeque<ModelProgress>>,
}

impl ProgressObserver {
    fn push(&self, origin: &Origin, purpose: &str, item: &Value, bytes: usize) {
        if !self.enabled.load(Ordering::Relaxed) || bytes > MODEL_PROGRESS_ITEM_BYTES {
            return;
        }
        // Observation must never hold up a model stream. Contention drops the item.
        if let Ok(mut queue) = self.queue.try_lock() {
            if queue.len() == MODEL_PROGRESS_RECORDS {
                queue.pop_front();
            }
            queue.push_back(ModelProgress {
                job_id: origin.job.clone(),
                call_id: origin.call.clone(),
                revision: origin.revision,
                purpose: purpose.to_owned(),
                item: item.clone(),
            });
        }
    }

    fn drain(&self, revision: u64, paused: bool) -> Vec<ModelProgress> {
        self.enabled.store(true, Ordering::Relaxed);
        match self.queue.try_lock() {
            Ok(mut queue) => queue
                .drain(..)
                .filter(|item| !paused && item.revision == revision)
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    fn clear(&self) {
        // The engine is the only consumer; a producer only holds this lock while cloning
        // one bounded item. Clearing is independent of task completion and persistence.
        if let Ok(mut queue) = self.queue.lock() {
            queue.clear();
        }
    }
}

/// Public operations are session-level. Job IDs are exposed in diagnostics only.
pub struct Engine {
    state: SessionState,
    store: Store,
    _lease: SessionLease,
    events: BTreeMap<String, Event>,
    order: Vec<String>,
    loaded: BTreeSet<String>,
    public_inputs: BTreeMap<u64, String>,
    notifications: VecDeque<Event>,
    model_progress: Arc<ProgressObserver>,
    tasks: JoinSet<Completed>,
    running: BTreeMap<String, Running>,
    write_lease: Option<WriteLease>,
    data_dir: PathBuf,
    profile: Profile,
    profile_name: String,
    options: RunOptions,
    started: Instant,
    faulted: bool,
}

impl Engine {
    pub fn open(
        data_dir: &Path,
        workspace: &Path,
        session_id: Option<&str>,
        profile: Profile,
        profile_name: String,
        options: RunOptions,
    ) -> Result<Self> {
        ensure!(
            options.max_calls > 0
                && options.max_jobs > 0
                && options.max_parallel > 0
                && options.context_chars >= 1024
                && options.model_timeout_seconds > 0,
            "execution limits must be positive (context_chars >= 1024)"
        );
        profile.validate()?;
        std::fs::create_dir_all(data_dir)?;
        let workspace = workspace.canonicalize().context("workspace must exist")?;
        ensure!(workspace.is_dir(), "workspace must be a directory");
        let store = Store::open(data_dir.join("sessions.sqlite3"))?;
        let (state, lease) = if let Some(id) = session_id {
            let lease = store.acquire_session(id)?;
            let prior = store.load_session(id)?;
            ensure!(
                prior.workspace == workspace,
                "session belongs to workspace {}",
                prior.workspace.display()
            );
            (store.recover_session(id)?, lease)
        } else {
            let state = SessionState::new(workspace);
            let lease = store.acquire_session(&state.id)?;
            store.create_session(&state)?;
            (state, lease)
        };
        let existing = store.event_metadata(&state.id)?;
        let public_inputs = existing
            .iter()
            .filter(|e| e.kind == "input" && e.data["source"] == "user")
            .map(|e| (e.revision, e.id.clone()))
            .collect();
        let order = existing.iter().map(|e| e.id.clone()).collect();
        let events = existing.into_iter().map(|e| (e.id.clone(), e)).collect();
        let mut engine = Self {
            state,
            store,
            _lease: lease,
            events,
            order,
            loaded: BTreeSet::new(),
            public_inputs,
            notifications: VecDeque::new(),
            model_progress: Arc::new(ProgressObserver::default()),
            tasks: JoinSet::new(),
            running: BTreeMap::new(),
            write_lease: None,
            data_dir: data_dir.to_owned(),
            profile,
            profile_name,
            options,
            started: Instant::now(),
            faulted: false,
        };
        engine.restore_working_set()?;
        engine.record_unknown_results()?;
        engine.clear_confirmed_marker()?;
        for write in engine.state.unknown_writes.values() {
            WriteLease::restore_unknown(
                &engine.state.workspace,
                &engine.state.id,
                &write.call_id,
                data_dir,
            )?;
        }
        Ok(engine)
    }

    pub fn state(&self) -> &SessionState {
        &self.state
    }

    pub fn options(&self) -> &RunOptions {
        &self.options
    }

    /// Explicit audit reads may load the whole history; scheduling never does.
    pub fn events(&self) -> Result<Vec<Event>> {
        self.store.events(&self.state.id)
    }

    pub fn read_event(&self, id: &str) -> Result<Event> {
        self.store.read_event(&self.state.id, id)
    }

    pub fn unanswered_questions(&self) -> Vec<&Event> {
        self.records()
            .filter(|event| self.is_unanswered_question(event))
            .collect()
    }

    fn records(&self) -> impl DoubleEndedIterator<Item = &Event> {
        self.order.iter().filter_map(|id| self.events.get(id))
    }

    pub fn post(&mut self, text: &str, reply_to: Option<&str>) -> Result<String> {
        self.ensure_healthy()?;
        ensure!(!text.trim().is_empty(), "message must not be empty");
        let question = if let Some(reply) = reply_to {
            let question = self
                .events
                .get(reply)
                .context("reply_to question does not exist")?;
            ensure!(
                question.kind == "question",
                "reply_to must identify a question"
            );
            ensure!(
                self.is_unanswered_question(question),
                "question has already been answered or its job is closed"
            );
            Some(question.clone())
        } else {
            self.records()
                .rev()
                .find(|event| self.is_unanswered_question(event))
                .cloned()
        };
        self.cancel_models("new user input")?;
        let mut state = self.state.clone();
        state.revision += 1;
        state.paused = false;
        // The latest input is the admission barrier. Its context includes all
        // earlier public instructions; their work stays in each job's queue.
        // Keeping an older barrier here would preempt the new tool workflow.
        state.pending_inputs.clear();
        let target = if let Some(question) = &question {
            question.job_id.clone().context("question has no job")?
        } else {
            match state.focus.as_ref().filter(|id| {
                state
                    .jobs
                    .get(*id)
                    .is_some_and(|j| j.state != JobState::Closed)
            }) {
                Some(id) => id.clone(),
                None => {
                    let job = Job::new("Conversation");
                    let id = job.id.clone();
                    state.jobs.insert(id.clone(), job);
                    id
                }
            }
        };
        state.focus = Some(target.clone());
        let mut input = self.event(
            &target,
            "input",
            json!({"message":Message::user(text),"source":"user"}),
        );
        input.revision = state.revision;
        input.root_input = Some(input.id.clone());
        input.reply_to = question.as_ref().map(|event| event.id.clone());
        let id = input.id.clone();
        let mut additions = vec![input];
        // Complete any outstanding question before adding the new native user turn.
        for pending in self.pending_tools(&target)? {
            if pending.call.function.name.as_str() == "ask_user"
                && question.as_ref().is_some_and(|event| {
                    event.data["tool_key"].as_str() == Some(pending.key.as_str())
                })
            {
                let event = self.tool_result(
                    &target,
                    &pending,
                    json!({"answer_input":id,"instruction":"The user's answer follows as the next user message."}),
                    None,
                    false,
                )?;
                state
                    .jobs
                    .get_mut(&target)
                    .unwrap()
                    .history
                    .push(event.id.clone());
                additions.push(event);
            }
        }
        let job = state
            .jobs
            .get_mut(&target)
            .context("reply target no longer exists")?;
        ensure!(
            job.state != JobState::Closed,
            "cannot reply to a closed job"
        );
        if job
            .current_call
            .as_ref()
            .is_some_and(|id| !self.running.contains_key(id))
        {
            job.current_call = None;
        }
        // A running external action finishes into its original input. Incoming work
        // is admitted immediately afterwards; its old proposal cannot start more tools.
        if job.current_call.is_none() {
            job.state = JobState::Ready;
        }
        job.inbox.push_back(id.clone());
        state.pending_inputs.push_front(id.clone());
        state.budgets.insert(
            id.clone(),
            Budget::new(self.options.max_calls, self.options.max_jobs),
        );
        self.commit(state, additions)?;
        Ok(id)
    }

    /// Begin observing and drain available native items. Items may be dropped under
    /// load; callers must render the committed response as the final result.
    pub fn drain_model_progress(&mut self) -> Vec<ModelProgress> {
        self.model_progress
            .drain(self.state.revision, self.state.paused)
    }

    pub fn stop(&mut self) -> Result<()> {
        self.ensure_healthy()?;
        let mut state = self.state.clone();
        state.revision += 1;
        state.paused = true;
        let mut additions = Vec::new();
        self.model_progress.clear();
        let running = std::mem::take(&mut self.running);
        for (_, task) in running {
            task.abort.abort();
            if let Some(tool) = task.tool {
                if tools::is_write(tool.call.function.name.as_str()) {
                    state.unknown_writes.insert(
                        task.origin.call.clone(),
                        UnknownWrite {
                            call_id: task.origin.call.clone(),
                            job_id: task.origin.job.clone(),
                            root_input: Some(task.origin.root.clone()),
                            tool_name: tool.call.function.name.to_string(),
                        },
                    );
                } else {
                    let result = self.tool_result(
                        &task.origin.job,
                        &tool,
                        json!({"cancelled":true}),
                        Some(&task.origin),
                        false,
                    )?;
                    state
                        .jobs
                        .get_mut(&task.origin.job)
                        .unwrap()
                        .history
                        .push(result.id.clone());
                    additions.push(result);
                }
            }
            let mut event = self.event(
                &task.origin.job,
                "cancelled",
                json!({"reason":"session stopped"}),
            );
            event.call_id = Some(task.origin.call);
            additions.push(event);
        }
        for job in state.jobs.values_mut() {
            job.current_call = None;
            if matches!(
                job.state,
                JobState::Ready | JobState::Running | JobState::Waiting
            ) {
                job.state = JobState::Paused;
            }
        }
        let mut event = Event::new(
            &state.id,
            "stopped",
            json!({"reason":"user stopped session"}),
        );
        event.revision = state.revision;
        additions.push(event);
        self.commit(state, additions)?;
        self.record_unknown_results()
    }

    pub fn resume(&mut self) -> Result<()> {
        self.ensure_healthy()?;
        if !self.state.paused
            && !self
                .state
                .jobs
                .values()
                .any(|j| j.state == JobState::Paused)
        {
            return Ok(());
        }
        self.cancel_models("resuming session")?;
        let mut state = self.state.clone();
        state.revision += 1;
        state.paused = false;
        let mut additions = Vec::new();
        for id in state.jobs.keys().cloned().collect::<Vec<_>>() {
            if state.jobs[&id].state != JobState::Paused {
                continue;
            }
            let blocked_active = state.jobs[&id]
                .active_input
                .as_deref()
                .is_some_and(|input| self.has_settled_ancestor(input));
            let has_independent = state.jobs[&id]
                .inbox
                .iter()
                .any(|input| !self.has_settled_ancestor(input));
            if (blocked_active || state.jobs[&id].active_input.is_none())
                && !has_independent
                && !state.jobs[&id].inbox.is_empty()
            {
                continue;
            }
            if blocked_active && !has_independent {
                continue;
            }
            // An interrupted read can safely be abandoned, not silently repeated.
            // A write's missing result remains blocked until explicit reconciliation.
            for pending in self.pending_tools(&id)? {
                if (blocked_active || self.was_started(&pending.key))
                    && !self.is_unknown_tool(&state, &pending)
                {
                    let event=self.tool_result(&id,&pending,json!({"interrupted":true,"instruction":"Inspect current state before deciding whether to try again."}),None,false)?;
                    state
                        .jobs
                        .get_mut(&id)
                        .unwrap()
                        .history
                        .push(event.id.clone());
                    additions.push(event);
                }
            }
            let job = state.jobs.get_mut(&id).unwrap();
            job.current_call = None;
            if blocked_active
                && let Some(previous) = job.active_input.take()
                && !job.inbox.contains(&previous)
            {
                job.inbox.push_back(previous);
            }
            job.state = if job.active_input.is_some() || !job.inbox.is_empty() {
                JobState::Ready
            } else {
                JobState::Idle
            };
        }
        let mut event = Event::new(&state.id, "resumed", json!({}));
        event.revision = state.revision;
        additions.push(event);
        self.commit(state, additions)
    }

    pub fn resolve_write(&mut self, call_id: &str, note: &str) -> Result<()> {
        self.ensure_healthy()?;
        ensure!(
            !note.trim().is_empty(),
            "record what was inspected and the observed outcome"
        );
        let unknown = self
            .state
            .unknown_writes
            .get(call_id)
            .context("unknown write was not found")?
            .clone();
        if let Some(lease) = &self.write_lease {
            lease.ensure_stopped()?;
        }
        // The child may inherit the same kernel file description. Drop the
        // parent's descriptor and reacquire independently before reconciliation.
        drop(self.write_lease.take());
        let lease = WriteLease::lock_existing(&self.state.workspace, &self.state.id, call_id)?;
        let mut state = self.state.clone();
        state.unknown_writes.remove(call_id);
        let mut event=self.event(&unknown.job_id,"tool_reconciled",json!({"message":Message::user(format!("An interrupted write was inspected. Observed outcome: {note}. Do not replay that operation automatically."))}));
        event.call_id = Some(call_id.to_owned());
        event.root_input = unknown.root_input;
        state
            .jobs
            .get_mut(&unknown.job_id)
            .unwrap()
            .history
            .push(event.id.clone());
        self.commit(state, vec![event])?;
        lease.clear()?;
        Ok(())
    }

    /// Replace the recipe used for future model calls while retaining the session lease.
    /// The caller controls whether and when paused work resumes.
    pub fn set_profile(&mut self, profile: Profile, profile_name: String) -> Result<()> {
        self.ensure_healthy()?;
        ensure!(
            self.is_quiescent(),
            "profile can only change while the engine is quiescent"
        );
        profile.validate()?;
        crate::config::validate_profile_name(&profile_name)?;
        self.profile = profile;
        self.profile_name = profile_name;
        Ok(())
    }

    pub fn is_quiescent(&self) -> bool {
        self.running.is_empty()
            && (self.state.paused || !self.state.jobs.values().any(|j| j.state == JobState::Ready))
    }

    /// Cancellation-safe: select this future against incoming user messages.
    /// Futures execute outside this method; no database transaction spans await.
    pub async fn step(&mut self) -> Result<Vec<Event>> {
        self.ensure_healthy()?;
        if !self.notifications.is_empty() {
            return Ok(self.notifications.drain(..).collect());
        }
        self.wake_waiters()?;
        if !self.state.paused {
            self.schedule()?;
        }
        if !self.notifications.is_empty() {
            return Ok(self.notifications.drain(..).collect());
        }
        if self.tasks.is_empty() {
            return Ok(Vec::new());
        }
        if let Some(result) = self.tasks.join_next().await {
            match result {
                Ok(done) => self.complete(done)?,
                Err(error) if error.is_cancelled() => {}
                Err(error) => {
                    self.poison();
                    return Err(error.into());
                }
            }
        }
        self.wake_waiters()?;
        Ok(self.notifications.drain(..).collect())
    }

    pub fn result(&self, input: &str) -> Option<&Event> {
        let terminal_id = self.terminal(input).map(|event| &event.id);
        self.records().rev().find(|e| {
            (matches!(
                e.kind.as_str(),
                "delivery" | "failure" | "input_paused" | "input_resolved"
            ) && e.reply_to.as_deref() == Some(input)
                && (e.kind != "failure" || terminal_id == Some(&e.id)))
                || (self.is_unanswered_question(e)
                    && (e.reply_to.as_deref() == Some(input)
                        || e.root_input.as_deref() == Some(input)))
        })
    }

    /// Questions belong to the public conversation even when a delegated job asks.
    pub fn is_unanswered_question(&self, event: &Event) -> bool {
        event.kind == "question"
            && event
                .job_id
                .as_ref()
                .and_then(|id| self.state.jobs.get(id))
                .is_some_and(|job| job.state != JobState::Closed)
            && event.data["tool_key"].as_str().is_some_and(|key| {
                !self.records().any(|result| {
                    result.kind == "tool_result" && result.data["tool_key"].as_str() == Some(key)
                })
            })
    }

    pub fn event_text(&self, event: &Event) -> Result<String> {
        if let Some(id) = event.data["response_event"].as_str() {
            let response_event = self.read_event(id)?;
            let response: CompletionResponse =
                serde_json::from_value(response_event.data["response"].clone())
                    .context("decoding delivery response")?;
            return Ok(response.text());
        }
        Ok(event.data["text"]
            .as_str()
            .or_else(|| event.data["question"].as_str())
            .or_else(|| event.data["error"].as_str())
            .or_else(|| event.data["reason"].as_str())
            .unwrap_or("")
            .to_owned())
    }

    pub fn metrics(&self, input: &str) -> Value {
        let matching = self
            .records()
            .filter(|e| e.root_input.as_deref() == Some(input))
            .collect::<Vec<_>>();
        let calls = matching
            .iter()
            .filter(|e| e.kind == "model_started")
            .filter_map(|e| e.call_id.as_deref())
            .collect::<Vec<_>>();
        let usage = matching
            .iter()
            .filter_map(|e| {
                Some((
                    e.call_id.as_deref()?,
                    e.data
                        .get("usage")
                        .or_else(|| e.data.get("response")?.get("usage"))?,
                ))
            })
            .collect::<BTreeMap<_, _>>();
        let total = |field: &str| -> Option<u64> {
            if calls.is_empty() {
                return None;
            }
            calls.iter().try_fold(0_u64, |sum, id| {
                sum.checked_add(usage.get(id)?[field].as_u64()?)
            })
        };
        let observed = |field: &str| -> Option<u64> {
            let values = usage
                .values()
                .filter_map(|u| u[field].as_u64())
                .collect::<Vec<_>>();
            if values.is_empty() {
                None
            } else {
                Some(values.into_iter().sum())
            }
        };
        json!({"model_calls":calls.len(),"input_tokens":total("input_tokens"),"output_tokens":total("output_tokens"),"total_tokens":total("total_tokens"),"observed_input_tokens":observed("input_tokens"),"observed_output_tokens":observed("output_tokens"),"cost":null,"elapsed_ms":self.started.elapsed().as_millis()})
    }

    fn working_ids(&self) -> BTreeSet<String> {
        self.state
            .jobs
            .values()
            .flat_map(|job| {
                let active_history = !matches!(job.state, JobState::Idle | JobState::Closed);
                job.history
                    .iter()
                    .filter(move |_| active_history)
                    .chain(job.active_input.iter())
                    .chain(job.inbox.iter())
                    .chain(job.summary.iter().filter(move |_| active_history))
            })
            .cloned()
            .collect()
    }

    fn restore_working_set(&mut self) -> Result<()> {
        // Old snapshots may retain a cumulative history. Remove only IDs whose
        // native contents are already represented by their committed summary.
        let mut state = self.state.clone();
        for job in state.jobs.values_mut() {
            if let Some(summary_id) = &job.summary {
                let summary = self.store.read_event(&state.id, summary_id)?;
                let covered: Vec<String> =
                    serde_json::from_value(summary.data["covered_ids"].clone())?;
                let covered: BTreeSet<_> = covered.into_iter().collect();
                job.history.retain(|id| {
                    !covered.contains(id)
                        || job.active_input.as_ref() == Some(id)
                        || job.inbox.contains(id)
                });
            }
            // An admitted input can enter history before older shared inputs.
            // Only a committed model start proves sharing already happened.
            let incorporated = self
                .records()
                .filter(|event| {
                    event.kind == "model_started" && event.job_id.as_deref() == Some(&job.id)
                })
                .map(|event| event.revision)
                .max()
                .unwrap_or(0);
            job.public_revision = job.public_revision.max(incorporated);
        }
        if state != self.state {
            self.store.commit(&state, &[])?;
            self.state = state;
        }
        self.refresh_working_set()
    }

    fn refresh_working_set(&mut self) -> Result<()> {
        let needed = self.working_ids();
        let missing = needed.difference(&self.loaded).cloned().collect();
        for event in self.store.read_events(&self.state.id, &missing)? {
            self.loaded.insert(event.id.clone());
            self.events.insert(event.id.clone(), event);
        }
        let obsolete = self.loaded.difference(&needed).cloned().collect::<Vec<_>>();
        for id in obsolete {
            if let Some(event) = self.events.get_mut(&id) {
                *event = event.metadata();
            }
            self.loaded.remove(&id);
        }
        Ok(())
    }

    fn ensure_healthy(&self) -> Result<()> {
        ensure!(
            !self.faulted,
            "session runtime lost persistence consistency; reopen the session before continuing"
        );
        Ok(())
    }

    fn poison(&mut self) {
        self.faulted = true;
        self.tasks.abort_all();
        for running in self.running.values() {
            running.abort.abort();
        }
    }

    fn commit(&mut self, state: SessionState, events: Vec<Event>) -> Result<()> {
        self.ensure_healthy()?;
        if let Err(error) = self.store.commit(&state, &events) {
            self.poison();
            return Err(error.context("session commit failed; reopen before continuing"));
        }
        self.state = state;
        let needed = self.working_ids();
        for event in events {
            if self.events.contains_key(&event.id) {
                continue;
            }
            self.order.push(event.id.clone());
            if event.kind == "input" && event.data["source"] == "user" {
                self.public_inputs.insert(event.revision, event.id.clone());
            }
            if needed.contains(&event.id) {
                self.loaded.insert(event.id.clone());
                self.events.insert(event.id.clone(), event.clone());
            } else {
                self.events.insert(event.id.clone(), event.metadata());
            }
            self.notifications.push_back(event);
        }
        if let Err(error) = self.refresh_working_set() {
            self.poison();
            return Err(
                error.context("working history could not be loaded; reopen before continuing")
            );
        }
        Ok(())
    }

    fn event(&self, job: &str, kind: &str, data: Value) -> Event {
        let mut e = Event::new(&self.state.id, kind, data);
        e.job_id = Some(job.to_owned());
        e.revision = self.state.revision;
        if let Some(input) = self
            .state
            .jobs
            .get(job)
            .and_then(|j| j.active_input.as_ref())
        {
            e.reply_to = Some(input.clone());
            e.root_input = self.events.get(input).and_then(|e| e.root_input.clone());
        }
        e
    }

    fn root(&self, input: &str) -> Result<String> {
        self.events
            .get(input)
            .and_then(|e| e.root_input.clone())
            .context("input has no budget identity")
    }

    fn has_settled_ancestor(&self, input: &str) -> bool {
        let Some(created) = self.order.iter().position(|id| id == input) else {
            return false;
        };
        let mut current = input;
        let mut seen = BTreeSet::new();
        while let Some(parent) = self
            .events
            .get(current)
            .and_then(|event| event.data["sender_input"].as_str())
        {
            if !seen.insert(parent) {
                return true;
            }
            // A new delegation after an earlier ancestor's settlement is fresh
            // Agent-authorized work. Block only retained inputs that existed
            // before their ancestor ended, using immutable append order.
            if self.terminal(parent).is_some_and(|event| {
                matches!(event.kind.as_str(), "delivery" | "input_resolved")
                    && self.order[created + 1..].contains(&event.id)
            }) {
                return true;
            }
            current = parent;
        }
        false
    }

    fn cancel_models(&mut self, reason: &str) -> Result<()> {
        let ids = self
            .running
            .iter()
            .filter(|(_, r)| r.tool.is_none())
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        if ids.is_empty() {
            return Ok(());
        }
        let mut state = self.state.clone();
        let mut events = Vec::new();
        for id in ids {
            let running = self.running.remove(&id).unwrap();
            running.abort.abort();
            let job = state.jobs.get_mut(&running.origin.job).unwrap();
            job.current_call = None;
            job.state = JobState::Ready;
            let mut event = self.event(&job.id, "model_cancelled", json!({"reason":reason}));
            event.call_id = Some(id);
            events.push(event);
        }
        self.commit(state, events)
    }

    fn schedule(&mut self) -> Result<()> {
        self.prepare_pending_input()?;
        let mut ids = self.state.jobs.keys().cloned().collect::<Vec<_>>();
        ids.sort_by_key(|id| self.state.focus.as_ref() != Some(id));
        for id in ids {
            if self.running.len() >= self.options.max_parallel {
                break;
            }
            if self.state.jobs[&id].state != JobState::Ready
                || self.state.jobs[&id].current_call.is_some()
            {
                continue;
            }
            if !self.state.pending_inputs.is_empty() && self.state.focus.as_ref() != Some(&id) {
                continue;
            }
            if self.state.jobs[&id].active_input.is_none() {
                let mut state = self.state.clone();
                let job = state.jobs.get_mut(&id).unwrap();
                job.active_input = job
                    .inbox
                    .iter()
                    .position(|input| !self.has_settled_ancestor(input))
                    .and_then(|index| job.inbox.remove(index));
                if let Some(input) = &job.active_input {
                    if !job.history.contains(input) {
                        job.history.push(input.clone());
                    }
                } else {
                    job.state = if job.inbox.is_empty() {
                        JobState::Idle
                    } else {
                        JobState::Paused
                    };
                }
                self.commit(state, Vec::new())?;
            }
            if self.state.jobs[&id].active_input.is_none() {
                continue;
            }
            let pending = self.pending_tools(&id)?;
            if let Some(tool) = pending.into_iter().next() {
                if self.proposal_is_stale(&tool)? {
                    let event=self.tool_result(&id,&tool,json!({"cancelled":true,"reason":"A newer user instruction invalidated this unstarted action. Read the latest session instructions and decide again."}),None,false)?;
                    self.append_history(&id, event)?;
                    continue;
                }
                if tools::is_external(tool.call.function.name.as_str()) {
                    self.start_tool(&id, tool)?;
                } else if let Err(error) = self.internal_tool(&id, &tool) {
                    let event = self.tool_result(
                        &id,
                        &tool,
                        json!({"error":format!("{error:#}")}),
                        None,
                        false,
                    )?;
                    self.append_history(&id, event)?;
                }
            } else {
                self.start_model(&id)?;
            }
        }
        Ok(())
    }

    fn pending_tools(&self, job: &str) -> Result<Vec<PendingTool>> {
        let Some(job) = self.state.jobs.get(job) else {
            return Ok(vec![]);
        };
        if matches!(job.state, JobState::Idle | JobState::Closed) {
            return Ok(vec![]);
        }
        let completed = job
            .history
            .iter()
            .filter_map(|id| self.events.get(id))
            .filter(|e| e.kind == "tool_result")
            .filter_map(|e| e.data["tool_key"].as_str())
            .collect::<BTreeSet<_>>();
        let mut pending = Vec::new();
        for id in &job.history {
            let Some(event) = self.events.get(id).filter(|e| e.kind == "model_message") else {
                continue;
            };
            let cold;
            let native = if self.loaded.contains(id) {
                event
            } else {
                cold = self.read_event(id)?;
                &cold
            };
            let response: CompletionResponse =
                serde_json::from_value(native.data["response"].clone())?;
            for call in response.tool_calls() {
                let key = format!("{}:{}", event.id, serde_json::to_string(&call.id)?);
                if !completed.contains(key.as_str()) {
                    pending.push(PendingTool {
                        call: call.clone(),
                        key,
                    });
                }
            }
        }
        Ok(pending)
    }

    fn prepare_pending_input(&mut self) -> Result<()> {
        let Some(input) = self.state.pending_inputs.front().cloned() else {
            return Ok(());
        };
        let owner = self
            .state
            .jobs
            .values()
            .find(|j| j.active_input.as_ref() == Some(&input) || j.inbox.contains(&input))
            .map(|j| j.id.clone())
            .context("pending input has no owner")?;
        if self.state.focus.as_ref() != Some(&owner) {
            let mut state = self.state.clone();
            state.focus = Some(owner.clone());
            self.commit(state, Vec::new())?;
        }
        let job = &self.state.jobs[&owner];
        if job.current_call.is_some() {
            return Ok(());
        }
        if job.active_input.as_ref() == Some(&input) {
            return Ok(());
        }
        // A native tool batch must be fully answered before inserting another user
        // turn. Abandon unstarted proposals; actual effects retain their results.
        for tool in self.pending_tools(&owner)? {
            let event=self.tool_result(&owner,&tool,json!({"cancelled":true,"reason":"A newer user input takes priority. Reconsider this action with the new instructions."}),None,false)?;
            self.append_history(&owner, event)?;
        }
        let mut state = self.state.clone();
        state.focus = Some(owner.clone());
        let job = state.jobs.get_mut(&owner).unwrap();
        if let Some(previous) = job.active_input.take()
            && !job.inbox.contains(&previous)
        {
            job.inbox.push_back(previous);
        }
        job.inbox.retain(|id| id != &input);
        job.active_input = Some(input.clone());
        job.wait_for.clear();
        job.state = JobState::Ready;
        if !job.history.contains(&input) {
            job.history.push(input);
        }
        self.commit(state, Vec::new())
    }

    fn record_unknown_results(&mut self) -> Result<()> {
        let unknown = self
            .state
            .unknown_writes
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for write in unknown {
            let start = self
                .records()
                .find(|e| e.kind == "tool_started" && e.call_id.as_deref() == Some(&write.call_id))
                .context("unknown write has no start record")?;
            let key = start.data["tool_key"]
                .as_str()
                .context("unknown write has no tool key")?
                .to_owned();
            if self
                .records()
                .any(|e| e.kind == "tool_result" && e.data["tool_key"].as_str() == Some(&key))
            {
                continue;
            }
            let pending = self
                .pending_tools(&write.job_id)?
                .into_iter()
                .find(|p| p.key == key)
                .context("unknown write has no native tool call")?;
            let event=self.tool_result(&write.job_id,&pending,json!({"effect":"unknown","instruction":"Inspect the current state. Further writes are blocked until the user records reconciliation; never repeat this operation automatically."}),None,true)?;
            self.append_history(&write.job_id, event)?;
        }
        Ok(())
    }

    fn clear_confirmed_marker(&self) -> Result<()> {
        let (_, marker) = workspace_write_paths(&self.state.workspace)?;
        if let Ok(bytes) = std::fs::read(&marker) {
            let info: Value = serde_json::from_slice(&bytes)?;
            if info["session_id"] == self.state.id
                && let Some(call) = info["call_id"].as_str()
            {
                let confirmed = self.records().any(|e| {
                    e.call_id.as_deref() == Some(call)
                        && ((e.kind == "tool_result" && e.data["uncertain"] != true)
                            || e.kind == "tool_reconciled")
                });
                if confirmed {
                    match WriteLease::lock_existing(&self.state.workspace, &self.state.id, call) {
                        Ok(lease) => lease.clear()?,
                        Err(error)
                            if error
                                .downcast_ref::<std::io::Error>()
                                .is_some_and(|e| e.kind() == std::io::ErrorKind::WouldBlock) => {}
                        Err(error) => return Err(error),
                    }
                }
            }
        }
        Ok(())
    }

    fn proposal_is_stale(&self, tool: &PendingTool) -> Result<bool> {
        let event_id = tool.key.split(':').next().context("invalid tool key")?;
        Ok(self
            .events
            .get(event_id)
            .is_none_or(|e| e.revision != self.state.revision))
    }
    fn was_started(&self, key: &str) -> bool {
        self.records()
            .any(|e| e.kind == "tool_started" && e.data["tool_key"].as_str() == Some(key))
    }
    fn is_unknown_tool(&self, state: &SessionState, tool: &PendingTool) -> bool {
        self.records().any(|e| {
            e.kind == "tool_started"
                && e.data["tool_key"].as_str() == Some(&tool.key)
                && e.call_id
                    .as_ref()
                    .is_some_and(|id| state.unknown_writes.contains_key(id))
        })
    }

    fn tool_result(
        &self,
        job: &str,
        tool: &PendingTool,
        content: Value,
        origin: Option<&Origin>,
        uncertain: bool,
    ) -> Result<Event> {
        let message = Message::tool_result(
            tool.call.id.clone(),
            tool.call.function.name.clone(),
            serde_json::to_string(&content)?,
        );
        let mut event=self.event(job,"tool_result",json!({"message":message,"tool_key":tool.key,"tool_name":tool.call.function.name,"uncertain":uncertain}));
        event.call_id = Some(
            self.records()
                .rev()
                .find(|e| {
                    e.kind == "tool_started" && e.data["tool_key"].as_str() == Some(&tool.key)
                })
                .and_then(|e| e.call_id.clone())
                .unwrap_or_else(|| tool.key.clone()),
        );
        if let Some(origin) = origin {
            event.call_id = Some(origin.call.clone());
            event.reply_to = Some(origin.input.clone());
            event.root_input = Some(origin.root.clone());
        }
        Ok(event)
    }
    fn append_history(&mut self, job: &str, event: Event) -> Result<()> {
        let mut state = self.state.clone();
        state
            .jobs
            .get_mut(job)
            .context("job missing")?
            .history
            .push(event.id.clone());
        self.commit(state, vec![event])
    }

    fn incorporate_public_inputs(&mut self, id: &str) -> Result<()> {
        let job = &self.state.jobs[id];
        let public = self
            .public_inputs
            .range((
                std::ops::Bound::Excluded(job.public_revision),
                std::ops::Bound::Unbounded,
            ))
            .map(|(_, event)| event)
            .filter(|event| !job.history.contains(event))
            .cloned()
            .collect::<Vec<_>>();
        let latest = self
            .public_inputs
            .last_key_value()
            .map(|(revision, _)| *revision)
            .unwrap_or(0);
        if !public.is_empty() || job.public_revision < latest {
            let mut state = self.state.clone();
            let job = state.jobs.get_mut(id).unwrap();
            job.history.extend(public);
            job.public_revision = latest;
            self.commit(state, Vec::new())?;
        }
        Ok(())
    }

    fn start_model(&mut self, id: &str) -> Result<()> {
        self.incorporate_public_inputs(id)?;
        let input = self.state.jobs[id].active_input.clone().unwrap();
        let root = self.root(&input)?;
        let history = context::build_history(&self.state.jobs[id], &self.events)?;
        let template = self.profile.apply(
            CompletionRequest::from(Vec::<Message>::new())
                .preamble(self.preamble(id)?)
                .tools(tools::definitions(
                    self.options.single_job,
                    self.options.read_only,
                )),
        );
        let overhead = context::serialized_chars(&template)?;
        let history_budget = self.options.context_chars.saturating_sub(overhead + 64);
        let summary_preamble = context::SUMMARY_PREAMBLE;
        let mut summary_template = self
            .profile
            .apply(CompletionRequest::from(Vec::<Message>::new()).preamble(summary_preamble));
        summary_template.max_tokens = Some(summary_template.max_tokens.unwrap_or(2048).min(2048));
        summary_template.tools.clear();
        summary_template.tool_choice = None;
        if let Some(params) = summary_template
            .additional_params
            .as_mut()
            .and_then(Value::as_object_mut)
        {
            params.remove("tools");
            params.remove("tool_choice");
        }
        // Reserve the final native task before selecting a source prefix.
        let mut summary_overhead = summary_template.clone();
        summary_overhead.chat_history.push(context::summary_task());
        let summary_budget = self
            .options
            .context_chars
            .saturating_sub(context::serialized_chars(&summary_overhead)? + 64);
        if history_budget == 0 || summary_budget == 0 {
            self.fail(
                id,
                "context limit is smaller than the model instructions, tools, and parameters",
            )?;
            return Ok(());
        }
        let mut request = template.clone();
        request.chat_history.extend(history);
        let compact = if context::serialized_chars(&request)? > self.options.context_chars {
            if self.options.no_compaction {
                self.fail(id, "context limit reached with compaction disabled")?;
                return Ok(());
            }
            match context::compaction_prefix(
                &self.state.jobs[id],
                &self.events,
                history_budget,
                summary_budget,
            )? {
                Some(prefix) => Some(prefix),
                None => {
                    match context::bounded_work_history(
                        &self.state.jobs[id],
                        &self.events,
                        history_budget,
                    )? {
                        Some(history) => {
                            request = template.clone();
                            request.chat_history.extend(history);
                            None
                        }
                        None => {
                            self.fail(id,"required inputs or native tool-call arguments cannot fit the configured context limit; use smaller source pages or smaller edits")?;
                            return Ok(());
                        }
                    }
                }
            }
        } else {
            None
        };
        let covered = if let Some((messages, covered)) = compact {
            request = summary_template;
            request.chat_history.extend(messages);
            request.chat_history.push(context::summary_task());
            Some(covered)
        } else {
            None
        };
        if context::serialized_chars(&request)? > self.options.context_chars {
            self.fail(
                id,
                "a complete model request cannot fit the configured context limit",
            )?;
            return Ok(());
        }
        let mut state = self.state.clone();
        if let Err(error) = state
            .budgets
            .get_mut(&root)
            .context("missing input budget")?
            .reserve_call()
        {
            self.fail(id, &error.to_string())?;
            return Ok(());
        }
        let origin = Origin {
            job: id.to_owned(),
            input,
            root,
            call: new_id(),
            revision: state.revision,
        };
        let job = state.jobs.get_mut(id).unwrap();
        job.state = JobState::Running;
        job.current_call = Some(origin.call.clone());
        let mut event=self.event(id,"model_started",json!({"purpose":if covered.is_some(){"summary"}else{"work"},"profile":self.profile_name}));
        event.call_id = Some(origin.call.clone());
        self.commit(state, vec![event])?;
        let profile = self.profile.clone();
        let data_dir = self.data_dir.clone();
        let name = self.profile_name.clone();
        let task_origin = origin.clone();
        let progress = Arc::clone(&self.model_progress);
        let purpose = if covered.is_some() { "summary" } else { "work" };
        let timeout = self.options.model_timeout_seconds;
        let abort = self.tasks.spawn(async move {
            let mut stream_items = Vec::new();
            let response = async {
                let prepared = model::prepare(
                    &profile,
                    &data_dir,
                    &name,
                    &task_origin.job,
                    &task_origin.call,
                )
                .await?;
                tokio::time::timeout(Duration::from_secs(timeout), async {
                    let connection = prepared.connect().await?;
                    let mut stream = connection
                        .model
                        .stream(request)
                        .map_err(|e| model::call_error_for_profile(&profile, &name, e))?;
                    let mut bytes = 0_usize;
                    while let Some(item) = stream.next().await {
                        let item =
                            item.map_err(|e| model::call_error_for_profile(&profile, &name, e))?;
                        let value = serde_json::to_value(item)?;
                        let item_bytes = serde_json::to_vec(&value)?.len();
                        bytes = bytes.saturating_add(item_bytes);
                        ensure!(
                            bytes <= 8 * 1024 * 1024,
                            "model stream exceeded the 8 MiB response limit"
                        );
                        progress.push(&task_origin, purpose, &value, item_bytes);
                        stream_items.push(value);
                    }
                    let response = stream
                        .finish()
                        .await
                        .map_err(|e| model::call_error_for_profile(&profile, &name, e))?;
                    Ok(response)
                })
                .await
                .unwrap_or_else(|_| Err(anyhow::anyhow!("model call timed out")))
            }
            .await;
            Completed::Model {
                origin: task_origin,
                response,
                covered,
                stream_items,
            }
        });
        self.running.insert(
            origin.call.clone(),
            Running {
                origin,
                abort,
                tool: None,
            },
        );
        Ok(())
    }

    fn start_tool(&mut self, id: &str, tool: PendingTool) -> Result<()> {
        self.clear_confirmed_marker()?;
        let name = tool.call.function.name.as_str();
        if self.options.read_only && tools::is_write(name) {
            let event = self.tool_result(
                id,
                &tool,
                json!({"error":"writes and shell are disabled for this session"}),
                None,
                false,
            )?;
            return self.append_history(id, event);
        }
        if tools::is_write(name) {
            if !self.state.unknown_writes.is_empty() {
                self.fail(id,"a previous write has unknown effects; inspect and reconcile it before further writes")?;
                return Ok(());
            }
            if self.write_lease.is_some() {
                return Ok(());
            }
        }
        let input = self.state.jobs[id].active_input.clone().unwrap();
        let origin = Origin {
            job: id.into(),
            root: self.root(&input)?,
            input,
            call: new_id(),
            revision: self.state.revision,
        };
        let lease = if tools::is_write(name) {
            match WriteLease::acquire(&self.state.workspace, &self.state.id, &origin.call) {
                Ok(Some(lease)) => Some(lease),
                Ok(None) => return Ok(()),
                Err(error) => {
                    self.fail(id, &format!("{error:#}"))?;
                    return Ok(());
                }
            }
        } else {
            None
        };
        let mut state = self.state.clone();
        let job = state.jobs.get_mut(id).unwrap();
        job.state = JobState::Running;
        job.current_call = Some(origin.call.clone());
        let mut event=self.event(id,"tool_started",json!({"tool_key":tool.key,"tool_name":name,"effect":if tools::is_write(name){"write"}else{"read"}}));
        event.call_id = Some(origin.call.clone());
        self.commit(state, vec![event])?;
        if let Some(write_guard) = &lease
            && let Err(error) = write_guard.arm(&self.state.id, &origin.call, &self.data_dir)
        {
            let event = self.tool_result(
                id,
                &tool,
                json!({"error":format!("write was not started: {error:#}")}),
                Some(&origin),
                false,
            )?;
            let mut state = self.state.clone();
            let job = state.jobs.get_mut(id).unwrap();
            job.current_call = None;
            job.state = JobState::Ready;
            job.history.push(event.id.clone());
            self.commit(state, vec![event])?;
            if let Some(lease) = lease {
                lease.clear()?;
            }
            return Ok(());
        }
        let task_write_lock = lease.as_ref().map(|lease| Arc::clone(&lease.file));
        let task_legacy_lock = lease.as_ref().map(|lease| Arc::clone(&lease.legacy_file));
        self.write_lease = lease.or(self.write_lease.take());
        let workspace = self.state.workspace.clone();
        let task_origin = origin.clone();
        let task_tool = tool.clone();
        let abort = self.tasks.spawn(async move {
            // Cancellation cannot interrupt synchronous file operations. Keep
            // workspace ownership until this future is actually dropped.
            let _write_lock = task_write_lock;
            let _legacy_lock = task_legacy_lock;
            let outcome = tools::execute(
                &workspace,
                task_tool.call.function.name.as_str(),
                &task_tool.call.function.arguments,
                _write_lock
                    .as_ref()
                    .zip(_legacy_lock.as_ref())
                    .map(|(stable, legacy)| [Arc::clone(stable), Arc::clone(legacy)]),
            )
            .await;
            Completed::Tool {
                origin: task_origin,
                tool: task_tool,
                outcome,
            }
        });
        if tools::is_write(name)
            && let Some(lease) = self.write_lease.as_mut()
        {
            lease.task = Some(abort.clone());
        }
        self.running.insert(
            origin.call.clone(),
            Running {
                origin,
                abort,
                tool: Some(tool),
            },
        );
        Ok(())
    }

    fn complete(&mut self, done: Completed) -> Result<()> {
        match done {
            Completed::Model {
                origin,
                response,
                covered,
                stream_items,
            } => {
                let stale = self.running.remove(&origin.call).is_none()
                    || origin.revision != self.state.revision
                    || self.state.paused;
                let mut state = self.state.clone();
                if let Some(job) = state
                    .jobs
                    .get_mut(&origin.job)
                    .filter(|j| !stale || j.current_call.as_ref() == Some(&origin.call))
                {
                    job.current_call = None;
                    job.state = if state.paused {
                        JobState::Paused
                    } else {
                        JobState::Ready
                    };
                }
                if stale {
                    let mut events = Vec::new();
                    if let Ok(response) = response {
                        let mut event =
                            self.event(&origin.job, "stale", json!({"response":response}));
                        event.call_id = Some(origin.call);
                        event.root_input = Some(origin.root);
                        events.push(event);
                    }
                    self.commit(state, events)?;
                    return Ok(());
                }
                self.commit(state, Vec::new())?;
                let response = match response {
                    Ok(response) => response,
                    Err(error) => {
                        let mut event = self.event(
                            &origin.job,
                            "model_failed",
                            json!({"error":format!("{error:#}"),"stream_items":stream_items}),
                        );
                        event.call_id = Some(origin.call);
                        self.commit(self.state.clone(), vec![event])?;
                        self.fail(&origin.job, &format!("{error:#}"))?;
                        return Ok(());
                    }
                };
                if response
                    .finish_reason()
                    .is_some_and(|r| r.truncated_output())
                {
                    let mut event = self.event(
                        &origin.job,
                        "model_failed",
                        json!({"response":response,"error":"truncated output"}),
                    );
                    event.call_id = Some(origin.call);
                    self.commit(self.state.clone(), vec![event])?;
                    self.fail(
                        &origin.job,
                        "model output was truncated; no proposed tools were executed",
                    )?;
                    return Ok(());
                }
                if let Some(covered) = covered {
                    ensure!(
                        response.tool_calls().next().is_none(),
                        "summary unexpectedly requested a tool"
                    );
                    if response.text().trim().is_empty() {
                        self.fail(&origin.job, "summary returned no text")?;
                        return Ok(());
                    }
                    let previous = self.state.jobs[&origin.job].summary.clone();
                    let mut event=self.event(&origin.job,"summary",json!({"response":response,"covered_ids":covered,"previous_summary":previous}));
                    event.call_id = Some(origin.call);
                    let mut state = self.state.clone();
                    let job = state.jobs.get_mut(&origin.job).unwrap();
                    let covered: BTreeSet<_> = covered.into_iter().collect();
                    job.history.retain(|id| {
                        !covered.contains(id)
                            || job.active_input.as_ref() == Some(id)
                            || job.inbox.contains(id)
                    });
                    job.summary = Some(event.id.clone());
                    self.commit(state, vec![event])?;
                    return Ok(());
                }
                let call_count = response.tool_calls().count();
                let invalid_batch = call_count > 1
                    && response
                        .tool_calls()
                        .any(|call| tools::is_control(call.function.name.as_str()));
                let text = response.text();
                let mut event =
                    self.event(&origin.job, "model_message", json!({"response":response}));
                event.call_id = Some(origin.call.clone());
                let response_event = event.id.clone();
                self.append_history(&origin.job, event)?;
                self.admit_input(&origin.job, &origin.input)?;
                if invalid_batch {
                    for pending in self.pending_tools(&origin.job)? {
                        let event=self.tool_result(&origin.job,&pending,json!({"error":"wait, handoff, ask_user and pause_work must be the only tool in a batch; no action from this batch was executed"}),None,false)?;
                        self.append_history(&origin.job, event)?;
                    }
                } else if call_count == 0 {
                    if text.trim().is_empty() {
                        self.fail(
                            &origin.job,
                            "model returned neither an answer nor a tool call",
                        )?;
                    } else {
                        let outstanding = self
                            .state
                            .jobs
                            .values()
                            .filter(|j| j.id != origin.job)
                            .flat_map(|j| j.active_input.iter().chain(j.inbox.iter()))
                            .filter(|input| {
                                self.events.get(*input).is_some_and(|e| {
                                    e.data["sender_input"].as_str() == Some(&origin.input)
                                }) && self.terminal(input).is_none()
                            })
                            .cloned()
                            .collect::<Vec<_>>();
                        if !outstanding.is_empty() {
                            let mut state = self.state.clone();
                            let job = state.jobs.get_mut(&origin.job).unwrap();
                            job.state = JobState::Waiting;
                            job.wait_for = outstanding;
                            if has_wait_cycle(&state, |input| self.terminal(input).is_some()) {
                                self.fail(&origin.job, "finishing this input would create a job dependency cycle; resolve the pending assignments first")?;
                            } else {
                                self.commit(state, Vec::new())?;
                            }
                        } else if !self.state.unknown_writes.is_empty() {
                            self.fail(
                                &origin.job,
                                "cannot deliver success while write effects remain unknown",
                            )?;
                        } else {
                            self.deliver(&origin.job, &response_event)?;
                        }
                    }
                }
            }
            Completed::Tool {
                origin,
                tool,
                outcome,
            } => {
                if self.running.remove(&origin.call).is_none() {
                    return Ok(());
                }
                let mut state = self.state.clone();
                let job = state.jobs.get_mut(&origin.job).unwrap();
                job.current_call = None;
                job.state = JobState::Ready;
                if outcome.uncertain {
                    state.unknown_writes.insert(
                        origin.call.clone(),
                        UnknownWrite {
                            call_id: origin.call.clone(),
                            job_id: origin.job.clone(),
                            root_input: Some(origin.root.clone()),
                            tool_name: tool.call.function.name.to_string(),
                        },
                    );
                    // A native placeholder records uncertainty without confirming the effect.
                    job.state = JobState::Paused;
                    let mut event = self.event(&origin.job, "write_unknown", outcome.content);
                    event.call_id = Some(origin.call);
                    self.commit(state, vec![event])?;
                    self.record_unknown_results()?;
                } else {
                    let event = self.tool_result(
                        &origin.job,
                        &tool,
                        outcome.content,
                        Some(&origin),
                        false,
                    )?;
                    job.history.push(event.id.clone());
                    if origin.revision != state.revision
                        && self.state.focus.as_ref() == Some(&origin.job)
                        && !job.inbox.is_empty()
                        && let Some(active) = job.active_input.take()
                    {
                        job.inbox.push_back(active);
                    }
                    self.commit(state, vec![event])?;
                    if tools::is_write(tool.call.function.name.as_str())
                        && let Some(lease) = self.write_lease.take()
                    {
                        lease.clear()?;
                    }
                }
            }
        }
        Ok(())
    }

    fn terminal(&self, input: &str) -> Option<&Event> {
        let event = self.records().rev().find(|e| {
            matches!(
                e.kind.as_str(),
                "delivery" | "failure" | "input_paused" | "input_resolved"
            ) && e.reply_to.as_deref() == Some(input)
        })?;
        if event.kind == "failure"
            && self.state.jobs.values().any(|j| {
                j.active_input.as_deref() == Some(input)
                    && matches!(
                        j.state,
                        JobState::Ready | JobState::Running | JobState::Waiting
                    )
            })
        {
            None
        } else {
            Some(event)
        }
    }

    fn admit_input(&mut self, job: &str, input: &str) -> Result<()> {
        if !self.state.pending_inputs.iter().any(|i| i == input) {
            return Ok(());
        }
        let mut state = self.state.clone();
        state.pending_inputs.retain(|i| i != input);
        let event = self.event(
            job,
            "input_handled",
            json!({"input_id":input,"instructions_revision":state.revision}),
        );
        // Every subsequent call includes current public user messages in its history.
        self.commit(state, vec![event])
    }

    fn deliver(&mut self, job: &str, response_event: &str) -> Result<()> {
        let event = self.event(job, "delivery", json!({"response_event":response_event}));
        let mut state = self.state.clone();
        let current = state.jobs.get_mut(job).unwrap();
        current.active_input = None;
        current.wait_for.clear();
        current.state = if current.inbox.is_empty() {
            JobState::Idle
        } else if current
            .inbox
            .iter()
            .any(|input| !self.has_settled_ancestor(input))
        {
            JobState::Ready
        } else {
            JobState::Paused
        };
        self.commit(state, vec![event])
    }

    fn fail(&mut self, job: &str, error: &str) -> Result<()> {
        let event = self.event(job, "failure", json!({"error":error}));
        let mut state = self.state.clone();
        let current = state.jobs.get_mut(job).context("failed job missing")?;
        current.current_call = None;
        current.state = JobState::Paused;
        // Keep the pending-input latch on failure; an old task may not resume work.
        self.commit(state, vec![event])
    }

    fn ensure_input_resolvable(&self, job: &str, input: &str) -> Result<()> {
        let root = self.root(input)?;
        ensure!(
            !self
                .state
                .unknown_writes
                .values()
                .any(|write| write.root_input.is_none()
                    || write.root_input.as_deref() == Some(&root)),
            "input has unresolved associated write effects"
        );
        ensure!(
            !self
                .running
                .values()
                .any(|running| running.origin.input == input),
            "input has an in-flight action"
        );
        for pending in self.pending_tools(job)? {
            let proposal = pending.key.split(':').next().context("invalid tool key")?;
            ensure!(
                self.events[proposal].reply_to.as_deref() != Some(input),
                "input has an unfinished native tool batch"
            );
        }
        // Descendants retain their exact input identities even if an intermediate
        // assignment failed. Settling an ancestor never cancels that work.
        let mut descendants = BTreeSet::from([input.to_owned()]);
        loop {
            let mut changed = false;
            for event in self.records().filter(|e| e.kind == "input") {
                if event.data["sender_input"]
                    .as_str()
                    .is_some_and(|sender| descendants.contains(sender))
                {
                    ensure!(
                        self.terminal(&event.id).is_some(),
                        "input has a live delegated assignment: {}",
                        event.id
                    );
                    ensure!(
                        !self
                            .state
                            .jobs
                            .values()
                            .any(|owner| owner.active_input.as_ref() == Some(&event.id)
                                || owner.inbox.contains(&event.id)),
                        "input has a retained delegated assignment: {}",
                        event.id
                    );
                    changed |= descendants.insert(event.id.clone());
                }
            }
            if !changed {
                break;
            }
        }
        Ok(())
    }

    fn internal_tool(&mut self, id: &str, tool: &PendingTool) -> Result<()> {
        let name = tool.call.function.name.as_str();
        let args = &tool.call.function.arguments;
        let input = self.state.jobs[id]
            .active_input
            .clone()
            .context("job has no active input")?;
        let root = self.root(&input)?;
        ensure!(
            !self.options.single_job
                || !matches!(
                    name,
                    "job_send" | "job_wait" | "job_handoff" | "job_close" | "job_control"
                ),
            "multiple jobs disabled for this ablation"
        );
        let mut state = self.state.clone();
        let mut events = Vec::new();
        let content = match name {
            "job_inspect" => self.inspect(args)?,
            "input_resolve" => {
                let resolutions = args["resolutions"]
                    .as_array()
                    .context("resolutions must be an array")?;
                ensure!(!resolutions.is_empty(), "resolutions must not be empty");
                let mut selected = BTreeSet::new();
                // Validate the entire batch before changing any queue or recording
                // a settlement. Ownership follows the current queue after handoff.
                for resolution in resolutions {
                    let target = tools::string_arg(resolution, "input_id")?;
                    ensure!(selected.insert(target), "duplicate input ID: {target}");
                    ensure!(target != input, "cannot resolve the ACTIVE input");
                    ensure!(
                        state.jobs[id].inbox.iter().any(|i| i == target),
                        "input is not queued in this job: {target}"
                    );
                    let original = self.events.get(target).context("input does not exist")?;
                    ensure!(original.kind == "input", "target is not an input");
                    let outcome = tools::string_arg(resolution, "outcome")?;
                    ensure!(
                        matches!(outcome, "completed" | "superseded"),
                        "invalid resolution outcome"
                    );
                    let reason = tools::string_arg(resolution, "reason")?;
                    ensure!(!reason.trim().is_empty(), "resolution reason is empty");
                    if let Some(evidence) = resolution.get("evidence_event_ids") {
                        for value in evidence
                            .as_array()
                            .context("evidence_event_ids must be an array")?
                        {
                            let evidence_id = value
                                .as_str()
                                .context("evidence event ID must be a string")?;
                            ensure!(
                                self.events.contains_key(evidence_id),
                                "evidence event does not exist: {evidence_id}"
                            );
                        }
                    }
                    self.ensure_input_resolvable(id, target)?;
                }
                let mut settled = Vec::new();
                for resolution in resolutions {
                    let target = resolution["input_id"].as_str().unwrap();
                    let mut event = self.event(id, "input_resolved", json!({
                        "outcome":resolution["outcome"], "reason":resolution["reason"],
                        "actor_input":input, "actor_job":id, "tool_key":tool.key,
                        "evidence_event_ids":resolution.get("evidence_event_ids").cloned().unwrap_or_else(|| json!([]))
                    }));
                    event.reply_to = Some(target.to_owned());
                    event.root_input = self.events[target].root_input.clone();
                    state
                        .jobs
                        .get_mut(id)
                        .unwrap()
                        .inbox
                        .retain(|i| i != target);
                    state.pending_inputs.retain(|i| i != target);
                    settled.push(json!({"input_id":target,"event_id":event.id,"outcome":resolution["outcome"],"reason":resolution["reason"]}));
                    events.push(event);
                }
                json!({"resolutions":settled})
            }
            "job_send" => {
                let message = tools::string_arg(args, "message")?;
                ensure!(!message.trim().is_empty(), "assignment is empty");
                let target = if let Some(target) = args["job_id"].as_str() {
                    ensure!(target != id, "send a new assignment to another job");
                    ensure!(
                        state
                            .jobs
                            .get(target)
                            .is_some_and(|j| j.state != JobState::Closed),
                        "job missing or closed"
                    );
                    target.to_owned()
                } else {
                    state
                        .budgets
                        .get_mut(&root)
                        .context("missing budget")?
                        .reserve_job()?;
                    let job = Job::new(args["title"].as_str().unwrap_or("Work"));
                    let target = job.id.clone();
                    state.jobs.insert(target.clone(), job);
                    target
                };
                let mut event=self.event(&target,"input",json!({"message":Message::user(message),"source":"job","sender_job":id,"sender_input":input}));
                event.reply_to = None;
                event.root_input = Some(root.clone());
                let assigned = event.id.clone();
                let resume_new_work = {
                    let target_job = &state.jobs[&target];
                    target_job.state == JobState::Paused
                        && target_job.current_call.is_none()
                        && (target_job
                            .active_input
                            .as_deref()
                            .is_some_and(|input| self.has_settled_ancestor(input))
                            || (target_job.active_input.is_none()
                                && target_job
                                    .inbox
                                    .iter()
                                    .any(|input| self.has_settled_ancestor(input))))
                };
                if resume_new_work {
                    for pending in self.pending_tools(&target)? {
                        if !self.is_unknown_tool(&state, &pending) {
                            let result = self.tool_result(&target, &pending,
                                json!({"cancelled":true,"reason":"Retained work belongs to an ended input. A newly authorized assignment follows; reconsider original proposals only after explicit retry."}), None, false)?;
                            state
                                .jobs
                                .get_mut(&target)
                                .unwrap()
                                .history
                                .push(result.id.clone());
                            events.push(result);
                        }
                    }
                }
                let job = state.jobs.get_mut(&target).unwrap();
                if resume_new_work
                    && let Some(previous) = job.active_input.take()
                    && !job.inbox.contains(&previous)
                {
                    job.inbox.push_front(previous);
                }
                job.inbox.push_back(assigned.clone());
                if job.state == JobState::Idle || resume_new_work {
                    job.state = JobState::Ready;
                }
                events.push(event);
                json!({"job_id":target,"input_id":assigned})
            }
            "job_wait" => {
                let inputs = args["input_ids"]
                    .as_array()
                    .context("input_ids must be an array")?
                    .iter()
                    .map(|v| {
                        v.as_str()
                            .map(str::to_owned)
                            .context("input ID must be a string")
                    })
                    .collect::<Result<Vec<_>>>()?;
                ensure!(!inputs.is_empty(), "wait needs at least one input");
                for requested in &inputs {
                    ensure!(
                        self.events
                            .get(requested)
                            .is_some_and(|e| e.kind == "input"),
                        "wait input does not exist"
                    );
                    ensure!(
                        requested != &input,
                        "job cannot wait on its own active input"
                    );
                }
                state.jobs.get_mut(id).unwrap().wait_for = inputs.clone();
                ensure!(
                    !has_wait_cycle(&state, |input| self.terminal(input).is_some()),
                    "waiting would create a job dependency cycle"
                );
                if inputs.iter().any(|i| self.terminal(i).is_none()) {
                    state.jobs.get_mut(id).unwrap().state = JobState::Waiting;
                    return self.commit(state, Vec::new());
                }
                state.jobs.get_mut(id).unwrap().wait_for.clear();
                self.wait_results(&inputs)?
            }
            "job_handoff" => {
                ensure!(
                    !self.records().any(|e| e.reply_to.as_deref() == Some(&input)
                        && e.job_id.as_deref() == Some(id)
                        && e.kind == "tool_started"),
                    "handoff must precede external calls"
                );
                ensure!(
                    !self.records().any(|e| e.kind == "input"
                        && e.data["sender_job"].as_str() == Some(id)
                        && e.root_input.as_deref() == Some(&root)),
                    "handoff must precede delegating work"
                );
                let target = if let Some(target) = args["job_id"].as_str() {
                    ensure!(target != id, "cannot hand off to self");
                    let job = state.jobs.get(target).context("target job missing")?;
                    ensure!(
                        job.state == JobState::Idle
                            && job.active_input.is_none()
                            && job.inbox.is_empty(),
                        "handoff target must be idle"
                    );
                    target.to_owned()
                } else {
                    state
                        .budgets
                        .get_mut(&root)
                        .context("missing budget")?
                        .reserve_job()?;
                    let job = Job::new(args["title"].as_str().unwrap_or("Conversation"));
                    let target = job.id.clone();
                    state.jobs.insert(target.clone(), job);
                    target
                };
                let source = state.jobs.get_mut(id).unwrap();
                source.active_input = None;
                source.state = if source.inbox.is_empty() {
                    JobState::Idle
                } else {
                    JobState::Ready
                };
                let job = state.jobs.get_mut(&target).unwrap();
                job.inbox.push_front(input.clone());
                job.state = JobState::Ready;
                state.focus = Some(target.clone());
                json!({"transferred":true,"job_id":target,"input_id":input})
            }
            "job_close" => {
                let target = tools::string_arg(args, "job_id")?;
                ensure!(target != id, "cannot close the currently executing job");
                let job = state.jobs.get_mut(target).context("job missing")?;
                ensure!(
                    job.state == JobState::Idle && job.inbox.is_empty(),
                    "only idle jobs can be closed"
                );
                job.state = JobState::Closed;
                json!({"closed":target})
            }
            "job_control" => {
                let target = tools::string_arg(args, "job_id")?;
                ensure!(target != id, "use pause_work to stop this job");
                let action = tools::string_arg(args, "state")?;
                ensure!(matches!(action, "paused" | "ready"), "invalid job state");
                let job = state.jobs.get_mut(target).context("job missing")?;
                ensure!(job.state != JobState::Closed, "job is closed");
                ensure!(
                    job.current_call.is_none(),
                    "job has an in-flight action; wait for it before changing state"
                );
                job.state = if action == "paused" {
                    JobState::Paused
                } else if job.active_input.is_some() || !job.inbox.is_empty() {
                    // The current Agent explicitly authorizes this retained work;
                    // automatic queue selection still skips settled ancestors.
                    if job.active_input.is_none() {
                        job.active_input = job.inbox.pop_front();
                        if let Some(input) = &job.active_input
                            && !job.history.contains(input)
                        {
                            job.history.push(input.clone());
                        }
                    }
                    JobState::Ready
                } else {
                    JobState::Idle
                };
                json!({"job_id":target,"state":job.state})
            }
            "ask_user" => {
                let question = tools::string_arg(args, "question")?;
                ensure!(!question.trim().is_empty(), "question is empty");
                state.jobs.get_mut(id).unwrap().state = JobState::Waiting;
                state.focus = Some(id.to_owned());
                let event = self.event(
                    id,
                    "question",
                    json!({"question":question,"tool_key":tool.key}),
                );
                return self.commit(state, vec![event]);
            }
            "pause_work" => {
                let result = self.tool_result(id, tool, json!({"paused":true}), None, false)?;
                let paused = self.event(
                    id,
                    "input_paused",
                    json!({"text":"Work paused.","tool_key":tool.key}),
                );
                let mut state = self.state.clone();
                state.pending_inputs.retain(|pending| pending != &input);
                let job = state.jobs.get_mut(id).unwrap();
                job.history.push(result.id.clone());
                job.active_input = None;
                job.wait_for.clear();
                job.state = if job.inbox.is_empty() {
                    JobState::Idle
                } else {
                    JobState::Ready
                };
                self.commit(state, vec![result, paused])?;
                return self.stop();
            }
            _ => bail!("unknown tool: {name}"),
        };
        let event = self.tool_result(id, tool, content, None, false)?;
        state
            .jobs
            .get_mut(id)
            .unwrap()
            .history
            .push(event.id.clone());
        events.push(event);
        self.commit(state, events)
    }

    fn readable_event(&self, event: &Event) -> Result<String> {
        if let Some(value) = event.data.get("response") {
            let response: CompletionResponse = serde_json::from_value(value.clone())?;
            let text = response.text();
            if !text.is_empty() {
                return Ok(text);
            }
            let calls = response
                .tool_calls()
                .map(|call| &call.function)
                .collect::<Vec<_>>();
            return Ok(serde_json::to_string(&calls)?);
        }
        if let Some(value) = event.data.get("message") {
            use rig_core::message::{ToolResultContent, UserContent};
            let message: Message = serde_json::from_value(value.clone())?;
            return match message {
                Message::System { content } => Ok(content),
                Message::User { content } => {
                    let mut parts = Vec::new();
                    for part in content {
                        match part {
                            UserContent::Text(text) => parts.push(text.text),
                            UserContent::ToolResult(result) => {
                                for content in result.content {
                                    match content {
                                        ToolResultContent::Text(text) => parts.push(text.text),
                                        ToolResultContent::Json {value} => parts.push(serde_json::to_string(&value)?),
                                        ToolResultContent::Image(_) => parts.push("[Image content; use raw=true for the original native event]".into()),
                                    }
                                }
                            }
                            _ => parts.push(
                                "[Non-text content; use raw=true for the original native event]"
                                    .into(),
                            ),
                        }
                    }
                    Ok(parts.join("\n"))
                }
                Message::Assistant { content, .. } => Ok(content
                    .iter()
                    .filter_map(|part| match part {
                        rig_core::completion::AssistantContent::Text(text) => {
                            Some(text.text.as_str())
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n")),
            };
        }
        self.event_text(event)
    }

    fn inspect(&self, args: &Value) -> Result<Value> {
        let budget = (self.options.context_chars / 8).clamp(512, 8_000);
        if let Some(event_id) = args["event_id"].as_str() {
            let event = self.read_event(event_id)?;
            if let Some(job_id) = args["job_id"].as_str() {
                ensure!(
                    event.job_id.as_deref() == Some(job_id),
                    "event does not belong to this job"
                );
            }
            let offset = args["offset"]
                .as_u64()
                .map(usize::try_from)
                .transpose()?
                .unwrap_or(0);
            let limit = args["limit"]
                .as_u64()
                .map(usize::try_from)
                .transpose()?
                .unwrap_or(budget / 2)
                .min(budget / 2)
                .max(1);
            let raw = args["raw"].as_bool().unwrap_or(false);
            let serialized = if raw {
                serde_json::to_string(&event)?
            } else {
                self.readable_event(&event)?
            };
            let total = serialized.chars().count();
            ensure!(offset <= total, "offset exceeds event length");
            let mut text = String::new();
            let mut count = 0;
            let mut escaped_chars = 2;
            for ch in serialized.chars().skip(offset).take(limit) {
                // JSON escapes can expand a chunk. Bound the actual tool content.
                let chars = serde_json::to_string(&ch.to_string())?.chars().count() - 2;
                if escaped_chars + chars > budget / 2 {
                    break;
                }
                text.push(ch);
                count += 1;
                escaped_chars += chars;
            }
            let next = offset + count;
            return Ok(
                json!({"event_id":event_id,"kind":event.kind,"revision":event.revision,"raw":raw,"offset":offset,"text":text,"total_chars":total,"next_offset":(next<total).then_some(next),"truncated":next<total}),
            );
        }
        if args["users_only"].as_bool().unwrap_or(false) {
            let before = if let Some(id) = args["before_id"].as_str() {
                let event = self.events.get(id).context("before_id does not exist")?;
                ensure!(
                    event.kind == "input" && event.data["source"] == "user",
                    "users_only before_id must be a user input"
                );
                event.revision
            } else {
                u64::MAX
            };
            let limit = args["limit"]
                .as_u64()
                .map(usize::try_from)
                .transpose()?
                .unwrap_or(8)
                .clamp(1, 32);
            let candidates = self
                .public_inputs
                .range(..before)
                .rev()
                .take(limit + 1)
                .map(|(_, id)| id)
                .collect::<Vec<_>>();
            let mut records = Vec::new();
            let mut size = 0;
            for id in candidates.iter().take(limit) {
                let event = self.read_event(id)?;
                let text = self.readable_event(&event)?;
                let preview = text
                    .chars()
                    .take((budget / 8).clamp(32, 384))
                    .collect::<String>();
                let record = json!({"id":event.id,"job_id":event.job_id,"revision":event.revision,"text":preview,"preview_truncated":preview.chars().count()<text.chars().count()});
                let chars = serde_json::to_string(&record)?.chars().count();
                if size + chars > budget / 2 {
                    break;
                }
                size += chars;
                records.push(record);
            }
            let truncated = candidates.len() > records.len();
            let next = truncated
                .then(|| records.last().and_then(|r| r["id"].as_str()))
                .flatten();
            return Ok(
                json!({"users_only":true,"records":records,"next_before_id":next,"truncated":truncated}),
            );
        }
        if let Some(job_id) = args["job_id"].as_str() {
            let job = self.state.jobs.get(job_id).context("unknown job")?;
            let limit = args["limit"]
                .as_u64()
                .map(usize::try_from)
                .transpose()?
                .unwrap_or(8)
                .clamp(1, 32);
            let records = self.store.job_records(
                &self.state.id,
                job_id,
                args["before_id"].as_str(),
                limit + 1,
            )?;
            let mut page = Vec::new();
            let mut size = 0;
            for event in records.iter().take(limit) {
                let record = json!({"id":event.id,"kind":event.kind,"revision":event.revision});
                let chars = serde_json::to_string(&record)?.chars().count();
                if size + chars > budget / 2 {
                    break;
                }
                size += chars;
                page.push(record);
            }
            let truncated = records.len() > page.len();
            let next = truncated
                .then(|| {
                    page.last()
                        .and_then(|v| v["id"].as_str())
                        .map(str::to_owned)
                })
                .flatten();
            return Ok(
                json!({"job_id":job_id,"title":job.title.chars().take(128).collect::<String>(),"state":job.state,"summary_event":job.summary,"records":page,"next_before_id":next,"truncated":truncated}),
            );
        }
        let mut jobs = Vec::new();
        let mut size = 0;
        for job in self.state.jobs.values() {
            let record = json!({"id":job.id,"title":job.title.chars().take(128).collect::<String>(),"state":job.state,"active_input":job.active_input,"queued_inputs":job.inbox.len()});
            let chars = serde_json::to_string(&record)?.chars().count();
            if size + chars > budget / 2 {
                break;
            }
            size += chars;
            jobs.push(record);
        }
        Ok(json!({"truncated":jobs.len()<self.state.jobs.len(),"jobs":jobs}))
    }

    fn wait_results(&self, inputs: &[String]) -> Result<Value> {
        let budget = (self.options.context_chars / 8).clamp(1024, 8_000);
        let per_result = (budget / inputs.len().max(1)).saturating_sub(256).min(800);
        let mut results = Vec::new();
        let mut size = 0;
        for id in inputs {
            if let Some(event) = self.terminal(id) {
                let text = self.event_text(event)?;
                let preview = text.chars().take(per_result).collect::<String>();
                let record = json!({"input_id":id,"event_id":event.id,"response_event":event.data.get("response_event"),"status":event.kind,"outcome":event.data.get("outcome"),"actor_input":event.data.get("actor_input"),"evidence_event_ids":event.data.get("evidence_event_ids"),"text":preview,"truncated":preview.chars().count()<text.chars().count()});
                let chars = serde_json::to_string(&record)?.chars().count();
                if size + chars > budget {
                    break;
                }
                size += chars;
                results.push(record);
            }
        }
        Ok(
            json!({"truncated":results.len()<inputs.len(),"results":results,"instruction":"Use job_inspect(event_id=...) to read the full original response or delivery. This is a bounded preview, not a replacement for verification."}),
        )
    }

    fn wake_waiters(&mut self) -> Result<()> {
        let ready = self
            .state
            .jobs
            .values()
            .filter(|j| {
                j.state == JobState::Waiting
                    && !j.wait_for.is_empty()
                    && j.wait_for.iter().all(|i| self.terminal(i).is_some())
            })
            .map(|j| j.id.clone())
            .collect::<Vec<_>>();
        for id in ready {
            let results = self.wait_results(&self.state.jobs[&id].wait_for)?;
            let pending = self
                .pending_tools(&id)?
                .into_iter()
                .find(|p| p.call.function.name.as_str() == "job_wait");
            let event = if let Some(pending) = pending {
                self.tool_result(&id, &pending, results, None, false)?
            } else {
                self.event(&id,"context_note",json!({"message":Message::user(format!("The requested inputs have settled. Inspect their outcomes before delivering; superseded work is not a successful assignment delivery: {results}"))}))
            };
            let mut state = self.state.clone();
            let job = state.jobs.get_mut(&id).unwrap();
            job.wait_for.clear();
            job.state = JobState::Ready;
            job.history.push(event.id.clone());
            self.commit(state, vec![event])?;
        }
        Ok(())
    }

    fn preamble(&self, id: &str) -> Result<String> {
        let job = &self.state.jobs[id];
        let catalog=self.state.jobs.values().take(32).map(|j|json!({"id":j.id,"title":j.title,"state":j.state,"active_input":j.active_input})).collect::<Vec<_>>();
        Ok(format!(
            "You are BONE, the one agent in this conversation. You are currently working inside job {id} ({title}). Jobs are your internal continuing work contexts; never ask the user to create, select or manage them. Every thought and action belongs to this job.\nYour current task is only the input marked status=ACTIVE with ID {input} in this request. Match that marker to its original content. QUEUED inputs are retained for later and must not replace the current task; HISTORICAL and SHARED inputs supply relevant background and corrections. Follow the newest applicable instruction by revision when older instructions conflict. Current input markers override any routing state described in an earlier summary. Use tools to inspect actual files and verify results. Reply with a final answer only after this input is handled. Preserve original constraints. When the ACTIVE request incorporates earlier work, inspect QUEUED requests and explicitly use input_resolve after verifying their work is completed or a newer instruction supersedes them. Give a concrete reason; leave independent queued work unresolved. A final answer settles only the ACTIVE input. Do not repeat completed or superseded effects.\nContinue related work here. Create other jobs only when independent work or a separate continuing context benefits the task. job_send returns an exact input_id; use job_wait instead of polling. You may send a followup to an existing idle job. Use job_handoff before acting to transfer conversation responsibility. Waiting and handoff must be sole tool calls in their batch. When the user asks to stop, call pause_work. To pause/resume other work after a changed instruction, use job_control. If instructions are unclear, ask_user.\nFile tools are confined to workspace {workspace}; shell has local user privileges. Do not claim an operation succeeded unless tool evidence verifies it. Tools disabled by the session permission policy must not be worked around.\nPublic user instructions are included in the history with their revisions. Apply newer relevant corrections to your work; other jobs' assignments remain theirs. When an earlier requirement or unfinished commitment is unclear in a summary, use job_inspect(users_only=true) to find original session instructions, then job_inspect(event_id=...) for their complete readable text. Follow next_before_id and next_offset when truncated. Do not conclude that a specification is missing just because its summary is vague.\nJOB CATALOG:\n{catalog}\nExecution budget shared by this input and its delegated jobs: {budget}",
            title = job.title,
            input = job.active_input.as_deref().unwrap_or(""),
            workspace = self.state.workspace.display(),
            catalog = serde_json::to_string(&catalog)?,
            budget = job
                .active_input
                .as_ref()
                .and_then(|i| self.root(i).ok())
                .and_then(|r| self.state.budgets.get(&r))
                .map(|b| format!(
                    "{} / {} calls; {} / {} created jobs",
                    b.calls_used, b.max_calls, b.jobs_used, b.max_jobs
                ))
                .unwrap_or_default()
        ))
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        if !self.faulted && !self.running.is_empty() {
            let _ = self.stop();
        }
        self.tasks.abort_all();
        for running in self.running.values() {
            running.abort.abort();
        }
    }
}

fn has_wait_cycle(state: &SessionState, is_terminal: impl Fn(&str) -> bool) -> bool {
    fn visits(
        id: &str,
        state: &SessionState,
        visiting: &mut BTreeSet<String>,
        done: &mut BTreeSet<String>,
        is_terminal: &impl Fn(&str) -> bool,
    ) -> bool {
        if done.contains(id) {
            return false;
        }
        if !visiting.insert(id.to_owned()) {
            return true;
        }
        if let Some(job) = state.jobs.get(id) {
            for input in &job.wait_for {
                if is_terminal(input) {
                    continue;
                }
                if let Some(owner) = state
                    .jobs
                    .values()
                    .find(|j| j.active_input.as_ref() == Some(input) || j.inbox.contains(input))
                    && visits(&owner.id, state, visiting, done, is_terminal)
                {
                    return true;
                }
            }
        }
        visiting.remove(id);
        done.insert(id.to_owned());
        false
    }
    let mut done = BTreeSet::new();
    state
        .jobs
        .keys()
        .any(|id| visits(id, state, &mut BTreeSet::new(), &mut done, &is_terminal))
}

pub(crate) fn workspace_write_paths(workspace: &Path) -> Result<(PathBuf, PathBuf)> {
    WriteLease::paths(workspace)
}

/// The lock spans the physical write and its durable completion. A crash leaves
/// a marker, so another session cannot blindly write over an unknown operation.
struct WriteLease {
    file: Arc<File>,
    marker: PathBuf,
    task: Option<AbortHandle>,
    legacy_file: Arc<File>,
    legacy_marker: PathBuf,
}
impl WriteLease {
    fn paths(workspace: &Path) -> Result<(PathBuf, PathBuf)> {
        let directory = crate::config::user_home()?.join(".bone/workspace-locks");
        std::fs::create_dir_all(&directory)?;
        let name = tools::sha256(workspace.as_os_str().as_encoded_bytes());
        Ok((
            directory.join(format!("{name}.lock")),
            directory.join(format!("{name}.pending")),
        ))
    }

    fn legacy_paths(workspace: &Path) -> Result<(PathBuf, PathBuf)> {
        let directory = std::env::temp_dir().join("bone-workspace-locks");
        std::fs::create_dir_all(&directory)?;
        let name = tools::sha256(workspace.as_os_str().as_encoded_bytes());
        Ok((
            directory.join(format!("{name}.lock")),
            directory.join(format!("{name}.pending")),
        ))
    }

    fn locked(workspace: &Path) -> Result<Self> {
        let (path, marker) = Self::paths(workspace)?;
        let (legacy_path, legacy_marker) = Self::legacy_paths(workspace)?;
        let open = |path: &Path| -> Result<File> {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(path)?;
            file.try_lock_exclusive()?;
            Ok(file)
        };
        // Keep compatibility ownership while an older executable uses its temp
        // lock. Never erase its unknown marker just because storage changed.
        let file = open(&path)?;
        let legacy_file = open(&legacy_path)?;
        let lease = Self {
            file: Arc::new(file),
            marker,
            task: None,
            legacy_file: Arc::new(legacy_file),
            legacy_marker,
        };
        if lease.legacy_marker.exists() {
            let legacy: Value = serde_json::from_slice(&std::fs::read(&lease.legacy_marker)?)?;
            if lease.marker.exists() {
                let stable: Value = serde_json::from_slice(&std::fs::read(&lease.marker)?)?;
                ensure!(
                    legacy == stable,
                    "stable and legacy workspace write markers disagree; inspect both before reconciliation"
                );
            } else {
                lease.arm_marker(&lease.marker, &legacy)?;
            }
        }
        Ok(lease)
    }

    fn acquire(workspace: &Path, _session: &str, _call: &str) -> Result<Option<Self>> {
        let lease = match Self::locked(workspace) {
            Ok(lease) => lease,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::WouldBlock) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        ensure!(
            !lease.marker.exists(),
            "workspace has an unconfirmed write; recover and reconcile the owning session before writing"
        );
        Ok(Some(lease))
    }

    fn restore_unknown(workspace: &Path, session: &str, call: &str, data_dir: &Path) -> Result<()> {
        let (_, marker) = Self::paths(workspace)?;
        if marker.exists() {
            return Ok(());
        }
        let lease = Self::locked(workspace)
            .context("workspace write is still running or cannot be recovered")?;
        if !lease.marker.exists() {
            lease.arm(session, call, data_dir)?;
        }
        Ok(())
    }

    fn arm(&self, session: &str, call: &str, data_dir: &Path) -> Result<()> {
        let info = json!({"session_id":session,"call_id":call,"store":data_dir.join("sessions.sqlite3").canonicalize().unwrap_or_else(|_|data_dir.join("sessions.sqlite3"))});
        self.arm_marker(&self.marker, &info)?;
        self.arm_marker(&self.legacy_marker, &info)
    }

    fn arm_marker(&self, marker: &Path, info: &Value) -> Result<()> {
        use std::io::Write;
        let temporary = marker.with_extension(format!("{}.tmp", new_id()));
        let result = (|| -> Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            file.write_all(serde_json::to_string(info)?.as_bytes())?;
            file.sync_all()?;
            std::fs::rename(&temporary, marker)?;
            File::open(marker.parent().context("marker parent")?)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(temporary);
        }
        result
    }
    fn clear(self) -> Result<()> {
        self.ensure_stopped()?;
        let markers = [self.marker.clone(), self.legacy_marker.clone()];
        drop(self);
        // A child process can outlive its Rust future and hold an inherited fd.
        // A fresh file description proves physical ownership was released.
        let mut locks = Vec::new();
        for marker in &markers {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(marker.with_extension("lock"))?;
            file.try_lock_exclusive()
                .context("workspace writer still owns the lock; wait before clearing its marker")?;
            locks.push(file);
        }
        for marker in &markers {
            if marker.exists() {
                std::fs::remove_file(marker).context("clearing confirmed workspace write")?;
                File::open(marker.parent().context("marker parent")?)?.sync_all()?;
            }
        }
        Ok(())
    }
    fn ensure_stopped(&self) -> Result<()> {
        ensure!(
            self.task.as_ref().is_none_or(AbortHandle::is_finished),
            "workspace write task has not stopped yet; wait before reconciling its effects"
        );
        ensure!(
            Arc::strong_count(&self.file) == 1 && Arc::strong_count(&self.legacy_file) == 1,
            "workspace write still owns its lock; wait before reconciling its effects"
        );
        Ok(())
    }
    fn lock_existing(workspace: &Path, session: &str, call: &str) -> Result<Self> {
        let lease = Self::locked(workspace).context("workspace write is still running")?;
        if lease.marker.exists() {
            let info: Value = serde_json::from_slice(&std::fs::read(&lease.marker)?)?;
            ensure!(
                info["session_id"] == session && info["call_id"] == call,
                "write belongs to another session"
            );
        }
        Ok(lease)
    }
}

#[cfg(test)]
#[path = "../tests/unit/runtime.rs"]
mod tests;
