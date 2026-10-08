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
use rig_core::completion::{CompletionRequest, CompletionResponse, FinishReason, Message};
use rig_core::message::ToolCall;
use rig_core::observe::{
    Action, AdapterContext, AdapterEvent, ObservationLog, ObservationTrace, Subject,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::task::{AbortHandle, JoinSet};

use crate::config::Profile;
use crate::state::{Budget, Event, Job, JobState, SessionState, new_id};
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
    cancellation: Option<tokio::sync::watch::Sender<bool>>,
    observations: Option<Arc<ObservationLog>>,
}

enum Completed {
    Model {
        origin: Origin,
        response: Result<CompletionResponse>,
        covered: Option<Vec<String>>,
        stream_items: Vec<Value>,
        observations: ObservationTrace,
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

/// Lossy live shell tails. Committed tool results remain the authoritative output.
#[derive(Clone, Debug, Serialize)]
pub struct ToolProgress {
    pub job_id: String,
    pub call_id: String,
    pub revision: u64,
    pub tool_name: String,
    pub stdout: String,
    pub stderr: String,
}

const TOOL_PROGRESS_BYTES: usize = 16 * 1024;
const TOOL_PROGRESS_CALLS: usize = 128;

struct ObservedTool {
    origin: Origin,
    name: String,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    dirty: bool,
}

#[derive(Default)]
struct ToolProgressObserver {
    enabled: AtomicBool,
    calls: Mutex<BTreeMap<String, ObservedTool>>,
}

impl ToolProgressObserver {
    fn push(&self, origin: &Origin, name: &str, stderr: bool, bytes: &[u8]) {
        if !self.enabled.load(Ordering::Relaxed) {
            return;
        }
        // A slow or contended observer must never delay draining process pipes.
        if let Ok(mut calls) = self.calls.try_lock() {
            if !calls.contains_key(&origin.call) && calls.len() == TOOL_PROGRESS_CALLS {
                calls.pop_first();
            }
            let call = calls
                .entry(origin.call.clone())
                .or_insert_with(|| ObservedTool {
                    origin: origin.clone(),
                    name: name.into(),
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                    dirty: false,
                });
            let tail = if stderr {
                &mut call.stderr
            } else {
                &mut call.stdout
            };
            tail.extend_from_slice(bytes);
            if tail.len() > TOOL_PROGRESS_BYTES {
                tail.drain(..tail.len() - TOOL_PROGRESS_BYTES);
            }
            call.dirty = true;
        }
    }
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
    tool_progress: Arc<ToolProgressObserver>,
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
        let store = Store::open(data_dir.join("sessions.sqlite"))?;
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
            tool_progress: Arc::new(ToolProgressObserver::default()),
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
        engine.refresh_working_set()?;
        engine.recover_tool_results()?;
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

    /// Read one original call record from this session without loading the whole transcript.
    pub fn read_call_event(&self, call_id: &str, kind: &str) -> Result<Event> {
        let event = self
            .records()
            .rev()
            .find(|event| event.call_id.as_deref() == Some(call_id) && event.kind == kind)
            .context("call record was not found in this session")?;
        self.read_event(&event.id)
    }

    pub fn unanswered_questions(&self) -> Vec<&Event> {
        self.records()
            .filter(|event| self.is_unanswered_question(event))
            .collect()
    }

    fn records(&self) -> impl DoubleEndedIterator<Item = &Event> {
        self.order.iter().filter_map(|id| self.events.get(id))
    }

    /// Post conversational input, automatically answering the latest pending question
    /// when no explicit target is supplied (the original CLI/API behavior).
    pub fn post(&mut self, text: &str, reply_to: Option<&str>) -> Result<String> {
        self.post_input(text, reply_to, true)
    }

    /// Post an independent user instruction even when a question is awaiting a reply.
    pub fn post_message(&mut self, text: &str) -> Result<String> {
        self.post_input(text, None, false)
    }

    fn post_input(
        &mut self,
        text: &str,
        reply_to: Option<&str>,
        automatic_reply: bool,
    ) -> Result<String> {
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
                "question is no longer awaiting an answer; choose another question or send an independent message"
            );
            Some(question.clone())
        } else if automatic_reply {
            self.records()
                .rev()
                .find(|event| self.is_unanswered_question(event))
                .cloned()
        } else {
            None
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

    /// Enable live shell observation and collect changed snapshots for owned calls.
    pub fn drain_tool_progress(&mut self) -> Vec<ToolProgress> {
        self.tool_progress.enabled.store(true, Ordering::Relaxed);
        let Ok(mut calls) = self.tool_progress.calls.try_lock() else {
            return Vec::new();
        };
        calls.retain(|id, call| {
            !self.faulted
                && !self.state.paused
                && call.origin.revision == self.state.revision
                && self
                    .state
                    .jobs
                    .get(&call.origin.job)
                    .and_then(|job| job.current_call.as_ref())
                    == Some(id)
        });
        calls
            .values_mut()
            .filter_map(|call| {
                if !std::mem::take(&mut call.dirty) {
                    return None;
                }
                Some(ToolProgress {
                    job_id: call.origin.job.clone(),
                    call_id: call.origin.call.clone(),
                    revision: call.origin.revision,
                    tool_name: call.name.clone(),
                    stdout: String::from_utf8_lossy(&call.stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&call.stderr).into_owned(),
                })
            })
            .collect()
    }

    pub fn stop(&mut self) -> Result<()> {
        self.ensure_healthy()?;
        let mut state = self.state.clone();
        state.revision += 1;
        state.paused = true;
        let mut additions = Vec::new();
        self.model_progress.clear();
        // External tools retain their futures and call ownership until their
        // actual result arrives. Cancelling the future would discard that fact.
        for task in self.running.values().filter(|task| task.tool.is_some()) {
            if let Some(cancellation) = &task.cancellation {
                let _ = cancellation.send(true);
            }
        }
        let models = self
            .running
            .iter()
            .filter(|(_, task)| task.tool.is_none())
            .map(|(call, _)| call.clone())
            .collect::<Vec<_>>();
        for call in models {
            let task = self.running.remove(&call).unwrap();
            task.abort.abort();
            state.jobs.get_mut(&task.origin.job).unwrap().current_call = None;
            let mut event = self.cancelled_model_event(&task, "session stopped");
            event.kind = "cancelled".into();
            additions.push(event);
        }
        for job in state.jobs.values_mut() {
            if job
                .current_call
                .as_ref()
                .is_some_and(|call| self.running.contains_key(call))
            {
                continue;
            }
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
        self.commit(state, additions)
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
            if state.jobs[&id]
                .current_call
                .as_ref()
                .is_some_and(|call| self.running.contains_key(call))
            {
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
            // Started calls receive their result on completion or recovery.
            // Abandon remaining interrupted proposals without repeating them.
            for pending in self.pending_tools(&id)? {
                if blocked_active || self.was_started(&pending.key) {
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

    /// Replace only the recipe captured by future model calls.
    pub fn set_profile(&mut self, profile: Profile, profile_name: String) -> Result<()> {
        self.ensure_healthy()?;
        profile.validate()?;
        crate::config::validate_profile_name(&profile_name)?;
        self.profile = profile;
        self.profile_name = profile_name;
        Ok(())
    }

    pub fn profile_recipe(&self) -> (&str, &Profile) {
        (&self.profile_name, &self.profile)
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
            let mut event = self.cancelled_model_event(&running, reason);
            event.call_id = Some(id);
            events.push(event);
        }
        self.commit(state, events)
    }

    fn cancelled_model_event(&self, running: &Running, reason: &str) -> Event {
        let mut data = running
            .observations
            .as_ref()
            .map(|log| generation_diagnostic(None, &log.trace()))
            .unwrap_or_else(|| json!({}));
        data["reason"] = json!(reason);
        data["purpose"] = self
            .read_call_event(&running.origin.call, "model_started")
            .ok()
            .map(|e| e.data["purpose"].clone())
            .unwrap_or(Value::Null);
        let mut event = self.event(&running.origin.job, "model_cancelled", data);
        event.call_id = Some(running.origin.call.clone());
        event.reply_to = Some(running.origin.input.clone());
        event.root_input = Some(running.origin.root.clone());
        event
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
                    .position(|input| {
                        !self.has_settled_ancestor(input)
                            && self
                                .events
                                .get(input)
                                .is_some_and(|event| event.data["source"] == "job")
                    })
                    .or_else(|| {
                        job.inbox
                            .iter()
                            .position(|input| !self.has_settled_ancestor(input))
                    })
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

    fn recover_tool_results(&mut self) -> Result<()> {
        let completed = self
            .records()
            .filter(|event| event.kind == "tool_result")
            .filter_map(|event| event.data["tool_key"].as_str().map(str::to_owned))
            .collect::<BTreeSet<_>>();
        let starts = self
            .records()
            .filter(|event| event.kind == "tool_started")
            .filter(|event| {
                event.data["tool_key"]
                    .as_str()
                    .is_some_and(|key| !completed.contains(key))
            })
            .cloned()
            .collect::<Vec<_>>();
        for start in starts {
            let key = start.data["tool_key"]
                .as_str()
                .context("started tool has no tool key")?
                .to_owned();
            let job = start.job_id.as_deref().context("started tool has no job")?;
            let proposal_id = key.split(':').next().context("invalid tool key")?;
            let proposal = self.read_event(proposal_id)?;
            let response: CompletionResponse =
                serde_json::from_value(proposal.data["response"].clone())?;
            let call = response
                .tool_calls()
                .find(|call| {
                    serde_json::to_string(&call.id)
                        .is_ok_and(|call_id| key == format!("{proposal_id}:{call_id}"))
                })
                .context("started tool has no native proposal")?
                .clone();
            let pending = PendingTool { call, key };
            let mut event = self.tool_result(
                job,
                &pending,
                json!({"interrupted":true,"effect":"unknown","instruction":"The prior runtime ended without a tool result. Inspect current state before deciding on a new action; never automatically replay this call."}),
                None,
                true,
            )?;
            // Recovery belongs to the original call/input, including when a
            // newer user message was queued before the process ended.
            event.call_id = start.call_id;
            event.reply_to = start.reply_to;
            event.root_input = start.root_input;
            self.append_history(job, event)?;
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

    /// Successful work and summary completions reset only their own phase.
    /// Input identity survives handoff, revision changes, stop and restart.
    fn limited_generation(&self, input: &str, purpose: &str) -> Option<&Event> {
        for event in self
            .records()
            .rev()
            .filter(|e| e.reply_to.as_deref() == Some(input))
        {
            if (purpose == "work" && event.kind == "model_message")
                || (purpose == "summary" && event.kind == "summary")
            {
                return None;
            }
            if event.kind == "model_limited" && event.data["purpose"] == purpose {
                return Some(event);
            }
        }
        None
    }

    fn recovery_started(&self, limited: &Event) -> bool {
        self.records().any(|event| {
            event.kind == "model_started"
                && event.data["recovery_of"].as_str() == limited.call_id.as_deref()
        })
    }

    fn work_template(&self, id: &str) -> Result<CompletionRequest> {
        Ok(self.profile.apply(
            CompletionRequest::from(Vec::<Message>::new())
                .preamble(self.preamble(id)?)
                .tools(tools::definitions(
                    self.options.single_job,
                    self.options.read_only,
                )),
        ))
    }

    fn start_model(&mut self, id: &str) -> Result<()> {
        self.incorporate_public_inputs(id)?;
        let input = self.state.jobs[id].active_input.clone().unwrap();
        let root = self.root(&input)?;
        let limited_work = self.limited_generation(&input, "work").cloned();
        if limited_work.as_ref().is_some_and(|event| {
            self.recovery_started(event) || event.data["retry_allowed"] == false
        }) {
            self.fail(id, "generation remained incomplete after the single continuation attempt; existing progress is preserved")?;
            return Ok(());
        }
        let mut history = context::build_history(&self.state.jobs[id], &self.events)?;
        let continuation_fact = limited_work.as_ref().map(|event| Message::user(format!(
                "[BONE RUNTIME FACT call={}] The previous generation reached a provider limit and was not accepted. None of its proposed tools or text were executed or delivered. Earlier committed tool results remain valid; do not repeat those actions. Continue the ACTIVE input under the latest instructions. Choose smaller complete actions: for long files, create a complete first portion with write_file mode=replace, then append complete portions using the latest returned SHA. Do not reconstruct or splice unfinished tool arguments. The configured generation settings and shared task allowance are unchanged.",
                event.call_id.as_deref().unwrap_or("unknown")
            )));
        if let Some(message) = &continuation_fact {
            history.push(message.clone());
        }
        let template = self.work_template(id)?;
        let overhead = context::serialized_chars(&template)?;
        let continuation_chars = continuation_fact
            .as_ref()
            .map(context::serialized_chars)
            .transpose()?
            .unwrap_or(0);
        let history_budget = self
            .options
            .context_chars
            .saturating_sub(overhead + continuation_chars + 64);
        let summary_preamble = context::SUMMARY_PREAMBLE;
        let mut summary_template = self
            .profile
            .apply(CompletionRequest::from(Vec::<Message>::new()).preamble(summary_preamble));
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
        let limited_summary = self.limited_generation(&input, "summary").cloned();
        let compact = if let Some(event) = &limited_summary {
            if self.recovery_started(event) || event.data["retry_allowed"] == false {
                self.fail(id, "summary generation remained incomplete after the single smaller-prefix attempt; original history and previous summary are preserved")?;
                return Ok(());
            }
            let failed = self.read_event(&event.id)?;
            let covered: Vec<String> = serde_json::from_value(failed.data["covered_ids"].clone())?;
            let source_chars = failed.data["source_chars"]
                .as_u64()
                .context("limited summary has no source size")?
                as usize;
            match context::smaller_compaction_prefix(
                &self.state.jobs[id],
                &self.events,
                &covered,
                source_chars,
            )? {
                Some(prefix) => Some(prefix),
                None => {
                    self.fail(id, "summary reached a generation limit and no smaller complete source prefix fits; original history and previous summary are preserved")?;
                    return Ok(());
                }
            }
        } else if context::serialized_chars(&request)? > self.options.context_chars {
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
                            request.chat_history.extend(continuation_fact.clone());
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
        let source_chars = compact
            .as_ref()
            .map(|(messages, _)| context::serialized_chars(messages))
            .transpose()?;
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
        let purpose = if covered.is_some() { "summary" } else { "work" };
        let recovery = if covered.is_some() {
            limited_summary.as_ref()
        } else {
            limited_work.as_ref()
        };
        let mut event = self.event(
            id,
            "model_started",
            json!({
                "purpose":purpose,"profile":self.profile_name,
                "requested_model":request.model.as_deref().or_else(|| self.profile.model_name()),
                "configured_generation":configured_generation(&request),
                "source_chars":source_chars,
                "recovery_of":recovery.and_then(|event|event.call_id.as_deref())
            }),
        );
        event.call_id = Some(origin.call.clone());
        self.commit(state, vec![event])?;
        let profile = self.profile.clone();
        let data_dir = self.data_dir.clone();
        let name = self.profile_name.clone();
        let task_origin = origin.clone();
        let progress = Arc::clone(&self.model_progress);
        let timeout = self.options.model_timeout_seconds;
        let log = Arc::new(ObservationLog::ring(256));
        let task_log = Arc::clone(&log);
        let abort = self.tasks.spawn(async move {
            let mut stream_items = Vec::new();
            let observation = AdapterContext::new(
                task_log.clone(),
                Subject::scoped(&task_origin.job),
                &task_origin.call,
            );
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
                        .stream_observed(request, observation)
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
                observations: task_log.drain(),
            }
        });
        self.running.insert(
            origin.call.clone(),
            Running {
                origin,
                abort,
                tool: None,
                cancellation: None,
                observations: Some(log),
            },
        );
        Ok(())
    }

    fn start_tool(&mut self, id: &str, tool: PendingTool) -> Result<()> {
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
        if tools::is_write(name) && self.write_lease.is_some() {
            return Ok(());
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
            match WriteLease::acquire(&self.state.workspace) {
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
        let task_write_lock = lease.as_ref().map(|lease| Arc::clone(&lease.file));
        self.write_lease = lease.or(self.write_lease.take());
        let workspace = self.state.workspace.clone();
        let task_origin = origin.clone();
        let task_tool = tool.clone();
        let progress = Arc::clone(&self.tool_progress);
        let observed_origin = origin.clone();
        let observed_name = name.to_owned();
        let observer: tools::ToolObserver = Arc::new(move |stderr, bytes| {
            progress.push(&observed_origin, &observed_name, stderr, bytes);
        });
        let (cancellation, cancelled) = tokio::sync::watch::channel(false);
        let abort = self.tasks.spawn(async move {
            // Cancellation cannot interrupt synchronous file operations. Keep
            // workspace ownership until this future is actually dropped.
            let _write_lock = task_write_lock;
            let outcome = tools::execute_with_progress(
                &workspace,
                task_tool.call.function.name.as_str(),
                &task_tool.call.function.arguments,
                _write_lock.clone(),
                Some(observer),
                Some(cancelled),
            )
            .await;
            Completed::Tool {
                origin: task_origin,
                tool: task_tool,
                outcome,
            }
        });
        self.running.insert(
            origin.call.clone(),
            Running {
                origin,
                abort,
                tool: Some(tool),
                cancellation: Some(cancellation),
                observations: None,
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
                observations,
            } => {
                let purpose = if covered.is_some() { "summary" } else { "work" };
                let diagnostic = generation_diagnostic(response.as_ref().ok(), &observations);
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
                    let mut data = diagnostic;
                    data["purpose"] = json!(purpose);
                    match response {
                        Ok(response) => data["response"] = json!(response),
                        Err(error) => data["error"] = json!(format!("{error:#}")),
                    }
                    let mut event = self.event(&origin.job, "stale", data);
                    event.call_id = Some(origin.call);
                    event.reply_to = Some(origin.input);
                    event.root_input = Some(origin.root);
                    events.push(event);
                    self.commit(state, events)?;
                    return Ok(());
                }
                let response = match response {
                    Ok(response) => response,
                    Err(error) => {
                        let error = if explicit_context_limit(&observations) {
                            format!(
                                "model context limit reached; existing progress and original history are preserved: {error:#}"
                            )
                        } else {
                            format!("{error:#}")
                        };
                        let mut data = diagnostic;
                        data["purpose"] = json!(purpose);
                        data["error"] = json!(error);
                        data["stream_items"] = json!(stream_items);
                        let mut event = self.event(&origin.job, "model_failed", data);
                        event.call_id = Some(origin.call);
                        self.commit(state, vec![event])?;
                        self.fail(&origin.job, &error)?;
                        return Ok(());
                    }
                };
                let rejection = if explicit_context_limit(&observations) {
                    Some("model context limit reached; existing progress and original history are preserved".to_owned())
                } else {
                    match response.finish_reason() {
                        Some(FinishReason::Length) => {
                            let prior = self.limited_generation(&origin.input, purpose);
                            let retry_allowed = prior.is_none();
                            let started = self.read_call_event(&origin.call, "model_started")?;
                            let mut data = diagnostic;
                            data["purpose"] = json!(purpose);
                            data["response"] = json!(response);
                            data["reason"] = json!("length");
                            data["retry_allowed"] = json!(retry_allowed);
                            data["covered_ids"] = json!(covered);
                            data["source_chars"] = started.data["source_chars"].clone();
                            let mut event = self.event(&origin.job, "model_limited", data);
                            event.call_id = Some(origin.call);
                            self.commit(state, vec![event])?;
                            if !retry_allowed {
                                self.fail(&origin.job, if purpose == "summary" {
                                    "summary generation remained incomplete after the single smaller-prefix attempt; original history and previous summary are preserved"
                                } else {
                                    "generation remained incomplete after the single continuation attempt; no proposal from either incomplete generation was executed, and existing progress is preserved"
                                })?;
                            }
                            return Ok(());
                        }
                        Some(FinishReason::ContentFilter) => Some("provider filtered the generation; this proposal was not executed, and existing progress is preserved".into()),
                        Some(FinishReason::Other(reason)) => Some(format!("provider ended generation with {reason}; this proposal was not accepted, and existing progress is preserved")),
                        Some(FinishReason::Stop | FinishReason::ToolCalls) | None => None,
                    }
                };
                if let Some(error) = rejection {
                    let mut data = diagnostic;
                    data["purpose"] = json!(purpose);
                    data["response"] = json!(response);
                    data["error"] = json!(error);
                    let mut event = self.event(&origin.job, "model_failed", data);
                    event.call_id = Some(origin.call);
                    self.commit(state, vec![event])?;
                    self.fail(&origin.job, &error)?;
                    return Ok(());
                }
                if let Some(covered) = covered {
                    let previous = self.state.jobs[&origin.job].summary.clone();
                    let mut data = diagnostic;
                    data["purpose"] = json!(purpose);
                    data["response"] = json!(response);
                    data["covered_ids"] = json!(covered);
                    data["previous_summary"] = json!(previous);
                    let mut event = self.event(&origin.job, "summary", data);
                    event.call_id = Some(origin.call);
                    let summary_error = if response.tool_calls().next().is_some() {
                        Some(
                            "summary unexpectedly requested a tool; original history and previous summary are preserved",
                        )
                    } else if response.text().trim().is_empty() {
                        Some(
                            "summary returned no text; original history and previous summary are preserved",
                        )
                    } else if !context::summary_shrinks_work(
                        &self.state.jobs[&origin.job],
                        &self.events,
                        &event,
                        &self.work_template(&origin.job)?,
                    )? {
                        Some(
                            "summary did not reduce the work request; original history and previous summary are preserved",
                        )
                    } else {
                        None
                    };
                    if let Some(error) = summary_error {
                        event.kind = "model_failed".into();
                        event.data["error"] = json!(error);
                        self.commit(state, vec![event])?;
                        self.fail(&origin.job, error)?;
                        return Ok(());
                    }
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
                let mut data = diagnostic;
                data["purpose"] = json!(purpose);
                data["response"] = json!(response);
                if call_count == 0 && text.trim().is_empty() {
                    let error = "model returned neither an answer nor a tool call; existing progress is preserved";
                    data["error"] = json!(error);
                    let mut event = self.event(&origin.job, "model_failed", data);
                    event.call_id = Some(origin.call);
                    self.commit(state, vec![event])?;
                    self.fail(&origin.job, error)?;
                    return Ok(());
                }
                let mut event = self.event(&origin.job, "model_message", data);
                event.call_id = Some(origin.call.clone());
                let response_event = event.id.clone();
                state
                    .jobs
                    .get_mut(&origin.job)
                    .unwrap()
                    .history
                    .push(event.id.clone());
                self.commit(state, vec![event])?;
                self.admit_input(&origin.job, &origin.input)?;
                if invalid_batch {
                    for pending in self.pending_tools(&origin.job)? {
                        let event=self.tool_result(&origin.job,&pending,json!({"error":"wait, handoff, ask_user and pause_work must be the only tool in a batch; no action from this batch was executed"}),None,false)?;
                        self.append_history(&origin.job, event)?;
                    }
                } else if call_count == 0 {
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
                    } else {
                        self.deliver(&origin.job, &response_event)?;
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
                let superseded = self.terminal(&origin.input).is_some_and(|event| {
                    event.kind == "input_resolved" && event.data["outcome"] == "superseded"
                });
                job.state = if superseded {
                    if job.active_input.as_ref() == Some(&origin.input) {
                        job.active_input = None;
                    }
                    self.queued_state(job)
                } else if state.paused || job.state == JobState::Paused {
                    JobState::Paused
                } else {
                    JobState::Ready
                };
                let event = self.tool_result(
                    &origin.job,
                    &tool,
                    outcome.content,
                    Some(&origin),
                    outcome.uncertain,
                )?;
                job.history.push(event.id.clone());
                if !superseded
                    && origin.revision != state.revision
                    && self.state.focus.as_ref() == Some(&origin.job)
                    && !job.inbox.is_empty()
                    && let Some(active) = job.active_input.take()
                {
                    job.inbox.push_back(active);
                }
                self.commit(state, vec![event])?;
                if tools::is_write(tool.call.function.name.as_str()) {
                    drop(self.write_lease.take());
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
        current.state = self.queued_state(current);
        self.commit(state, vec![event])
    }

    // Automatic continuation is for delegated work. Retained user messages
    // require a new instruction or an explicit resume, even in mixed queues.
    fn queued_state(&self, job: &Job) -> JobState {
        if job.inbox.is_empty() {
            JobState::Idle
        } else if job.inbox.iter().any(|input| {
            !self.has_settled_ancestor(input)
                && self
                    .events
                    .get(input)
                    .is_some_and(|event| event.data["source"] == "job")
        }) {
            JobState::Ready
        } else {
            JobState::Paused
        }
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
        // Completion must account for delegated work. Supersession instead
        // cancels its outstanding proposals, while started effects still settle.
        for descendant in self
            .descendant_inputs(input)
            .into_iter()
            .filter(|id| id != input)
        {
            ensure!(
                self.terminal(&descendant).is_some(),
                "input has a live delegated assignment: {descendant}"
            );
            ensure!(
                !self
                    .state
                    .jobs
                    .values()
                    .any(|owner| owner.active_input.as_ref() == Some(&descendant)
                        || owner.inbox.contains(&descendant)),
                "input has a retained delegated assignment: {descendant}"
            );
        }
        Ok(())
    }

    fn descendant_inputs(&self, input: &str) -> BTreeSet<String> {
        let mut descendants = BTreeSet::from([input.to_owned()]);
        loop {
            let mut changed = false;
            for event in self.records().filter(|e| e.kind == "input") {
                if event.data["sender_input"]
                    .as_str()
                    .is_some_and(|sender| descendants.contains(sender))
                {
                    changed |= descendants.insert(event.id.clone());
                }
            }
            if !changed {
                break;
            }
        }
        descendants
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
        let mut cancel_calls = Vec::new();
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
                    if outcome == "completed" {
                        self.ensure_input_resolvable(id, target)?;
                    }
                }
                let superseded = resolutions
                    .iter()
                    .filter(|resolution| resolution["outcome"] == "superseded")
                    .flat_map(|resolution| {
                        self.descendant_inputs(resolution["input_id"].as_str().unwrap())
                    })
                    .collect::<BTreeSet<_>>();
                // Preserve native call/result pairs for unstarted proposals. The
                // running tools retain their call ownership until real completion.
                for owner in self.state.jobs.values() {
                    for pending in self.pending_tools(&owner.id)? {
                        if pending.key == tool.key {
                            continue;
                        }
                        let proposal = &self.events[pending.key.split(':').next().unwrap()];
                        if !proposal
                            .reply_to
                            .as_ref()
                            .is_some_and(|input| superseded.contains(input))
                            || self.was_started(&pending.key)
                        {
                            continue;
                        }
                        let mut result = self.tool_result(&owner.id, &pending,
                            json!({"cancelled":true,"reason":"A newer instruction superseded this input."}), None, false)?;
                        result.reply_to = proposal.reply_to.clone();
                        result.root_input = proposal.root_input.clone();
                        state
                            .jobs
                            .get_mut(&owner.id)
                            .unwrap()
                            .history
                            .push(result.id.clone());
                        events.push(result);
                    }
                    let job = state.jobs.get_mut(&owner.id).unwrap();
                    job.inbox.retain(|input| !superseded.contains(input));
                    if job
                        .active_input
                        .as_ref()
                        .is_some_and(|input| superseded.contains(input))
                    {
                        job.wait_for.clear();
                        job.state = JobState::Paused;
                        if job.current_call.is_none() {
                            job.active_input = None;
                            job.state = self.queued_state(job);
                        }
                    }
                }
                for (call, running) in self
                    .running
                    .iter()
                    .filter(|(_, running)| superseded.contains(&running.origin.input))
                {
                    cancel_calls.push(call.clone());
                    if running.tool.is_none() {
                        let origin = &running.origin;
                        let job = state.jobs.get_mut(&origin.job).unwrap();
                        job.current_call = None;
                        job.active_input = None;
                        job.state = self.queued_state(job);
                        let mut event = self.cancelled_model_event(running, "input superseded");
                        event.call_id = Some(call.clone());
                        event.reply_to = Some(origin.input.clone());
                        event.root_input = Some(origin.root.clone());
                        events.push(event);
                    }
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
                // A waiter for a delegated input must receive its exact outcome,
                // even when the original assignment never reached a delivery.
                for target in superseded
                    .iter()
                    .filter(|target| !selected.contains(target.as_str()))
                {
                    if self
                        .terminal(target)
                        .is_some_and(|event| event.kind == "delivery")
                    {
                        continue;
                    }
                    let original = &self.events[target];
                    let mut event = self.event(original.job_id.as_deref().unwrap(), "input_resolved",
                        json!({"outcome":"superseded","reason":"The originating user request was superseded.","actor_input":input,"actor_job":id,"tool_key":tool.key}));
                    event.reply_to = Some(target.clone());
                    event.root_input = original.root_input.clone();
                    events.push(event);
                }
                state
                    .pending_inputs
                    .retain(|input| !superseded.contains(input));
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
                source.state = self.queued_state(source);
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
        self.commit(state, events)?;
        for call in cancel_calls {
            if let Some(running) = self.running.get(&call) {
                if let Some(signal) = &running.cancellation {
                    let _ = signal.send(true);
                } else {
                    self.running.remove(&call).unwrap().abort.abort();
                }
            }
        }
        Ok(())
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
            "You are BONE, the one agent in this conversation. You are currently working inside job {id} ({title}). Jobs are your internal continuing work contexts; never ask the user to create, select or manage them. Every thought and action belongs to this job.\nYour current task is only the input marked status=ACTIVE with ID {input} in this request. Match that marker to its original content. QUEUED inputs are retained for later and must not replace the current task; HISTORICAL and SHARED inputs supply relevant background and corrections. Follow the newest applicable instruction by revision when older instructions conflict. Current input markers override any routing state described in an earlier summary. Use tools to inspect actual files and verify results. Reply with a final answer only after this input is handled. Preserve original constraints. When the ACTIVE request incorporates earlier work, inspect QUEUED requests and explicitly use input_resolve after verifying their work is completed or a newer instruction supersedes them. Give a concrete reason; leave independent queued work unresolved. A final answer settles only the ACTIVE input. Older user inputs remain available but do not automatically run after it. For an explanation-only request, explain and leave the earlier work paused; resume it only when authorized. Do not repeat completed or superseded effects. Interrupted tool results are ordinary execution facts: inspect actual state with tools and continue under the current instruction. Ask the user only for information or authorization you actually lack; do not ask them to reconcile calls.\nContinue related work here. Create other jobs only when independent work or a separate continuing context benefits the task. job_send returns an exact input_id; use job_wait instead of polling. You may send a followup to an existing idle job. Use job_handoff before acting to transfer conversation responsibility. Waiting and handoff must be sole tool calls in their batch. When the user asks to stop, call pause_work. To pause/resume other work after a changed instruction, use job_control. If instructions are unclear, ask_user.\nFile tools are confined to workspace {workspace}; shell has local user privileges. Do not claim an operation succeeded unless tool evidence verifies it. Tools disabled by the session permission policy must not be worked around.\nPublic user instructions are included in the history with their revisions. Apply newer relevant corrections to your work; other jobs' assignments remain theirs. When an earlier requirement or unfinished commitment is unclear in a summary, use job_inspect(users_only=true) to find original session instructions, then job_inspect(event_id=...) for their complete readable text. Follow next_before_id and next_offset when truncated. Do not conclude that a specification is missing just because its summary is vague.\nJOB CATALOG:\n{catalog}\nExecution budget shared by this input and its delegated jobs: {budget}",
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

/// Only generation parameters are projected; arbitrary extension fields can
/// carry user metadata or credentials and do not belong in diagnostics.
fn configured_generation(request: &CompletionRequest) -> Value {
    let mut params = serde_json::Map::new();
    if let Some(extra) = request
        .additional_params
        .as_ref()
        .and_then(Value::as_object)
    {
        for key in [
            "max_tokens",
            "max_output_tokens",
            "max_completion_tokens",
            "temperature",
            "top_p",
            "top_k",
            "reasoning_effort",
        ] {
            if let Some(value) = extra
                .get(key)
                .filter(|v| v.is_number() || v.is_string() || v.is_null())
            {
                let value = if let Some(text) = value.as_str() {
                    json!(rig_core::observe::scrub_diagnostic(text, &[]))
                } else {
                    value.clone()
                };
                params.insert(key.into(), value);
            }
        }
        for key in ["reasoning", "thinking"] {
            if let Some(extra) = extra.get(key).and_then(Value::as_object) {
                let mut selected = serde_json::Map::new();
                for field in ["effort", "summary", "type", "budget_tokens", "max_tokens"] {
                    if let Some(value) = extra
                        .get(field)
                        .filter(|v| v.is_number() || v.is_string() || v.is_null())
                    {
                        let value = if let Some(text) = value.as_str() {
                            json!(rig_core::observe::scrub_diagnostic(text, &[]))
                        } else {
                            value.clone()
                        };
                        selected.insert(field.into(), value);
                    }
                }
                params.insert(key.into(), Value::Object(selected));
            }
        }
    }
    json!({"max_tokens":request.max_tokens,"temperature":request.temperature,"additional_params":params})
}

fn explicit_context_limit(trace: &ObservationTrace) -> bool {
    let context_code = |code: &str| {
        matches!(
            code,
            "model_length" | "model_context_window_exceeded" | "context_length_exceeded"
        )
    };
    trace
        .observations
        .iter()
        .any(|observation| match &observation.action {
            Action::Adapter { observation } => match &observation.event {
                AdapterEvent::Provider { verdict } => {
                    verdict.finish_reason.as_deref().is_some_and(context_code)
                        || verdict.detail.as_deref().is_some_and(context_code)
                }
                AdapterEvent::ErrorEnvelope { error } => {
                    error.code.as_deref().is_some_and(context_code)
                }
                _ => false,
            },
            _ => false,
        })
}

fn generation_diagnostic(response: Option<&CompletionResponse>, trace: &ObservationTrace) -> Value {
    let usage = trace
        .observations
        .iter()
        .rev()
        .find_map(|observation| match &observation.action {
            Action::Adapter { observation } => match &observation.event {
                AdapterEvent::Usage { usage } => Some(usage),
                _ => None,
            },
            _ => None,
        });
    let reported_cap = response
        .and_then(|r| r.raw.get("max_output_tokens"))
        .and_then(Value::as_u64);
    // Native response usage is authoritative when present. A failed native fold
    // can still have reported usage in the provider observation.
    json!({
        "finish_reason":response.and_then(CompletionResponse::finish_reason),
        "usage":response.map(|r| json!(r.usage)).unwrap_or_else(||json!(usage)),
        "provider_reported":{"usage":usage,"max_output_tokens":reported_cap},
        "observations":trace
    })
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

/// Physical ownership lasts as long as the runtime task or shell descendants.
struct WriteLease {
    file: Arc<File>,
}
impl WriteLease {
    fn path(workspace: &Path) -> Result<PathBuf> {
        let directory = crate::config::user_home()?.join(".bone/workspace-locks");
        std::fs::create_dir_all(&directory)?;
        let name = tools::sha256(workspace.as_os_str().as_encoded_bytes());
        Ok(directory.join(format!("{name}.lock")))
    }

    fn locked(workspace: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(Self::path(workspace)?)?;
        file.try_lock_exclusive()?;
        #[cfg(windows)]
        crate::windows::ensure_writer_stopped(&file)?;
        Ok(Self {
            file: Arc::new(file),
        })
    }

    fn acquire(workspace: &Path) -> Result<Option<Self>> {
        match Self::locked(workspace) {
            Ok(lease) => Ok(Some(lease)),
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(crate::filesystem::lock_contended) =>
            {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
#[path = "../tests/unit/runtime.rs"]
mod tests;
