//! Process-local Agent runtime attachments, connection/start effects, and
//! tagged observation fan-in.
//!
//! Durable session ownership, journal writes, and recovery remain in the
//! session controller. This module deliberately owns only resources that end
//! with the current BONE process: Agent handles and their observer tasks.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::{ChatGptCredentials, WorkspaceApplication};
use bone_agent::{
    AgentHandle, AgentHost, Observation, ResolvedAgentRuntimeConfig, Snapshot, StepEvent,
};
use bone_llm::service::chatgpt_subscription::{self, DeviceCodePrompt};
use futures_util::stream::FuturesUnordered;
use tokio::{
    sync::{broadcast::error::RecvError, mpsc},
    task::JoinHandle,
};

use super::{
    app::{App, AppEvent, UiSessionId},
    report_notice,
    session_controller::{
        DurableUiSession, PendingRuntimeTurn, persist_runtime_retryable, persist_runtime_starting,
    },
};

pub(super) type ConnectionTask = JoinHandle<Result<AgentHost, String>>;
pub(super) type StartTask = JoinHandle<(UiSessionId, Result<(AgentHandle, Observation), String>)>;

pub(super) struct EnqueuedPendingStarts {
    ids: Vec<UiSessionId>,
    notices: Vec<String>,
}

pub(super) struct LiveSession {
    pub(super) id: UiSessionId,
    pub(super) agent: AgentHandle,
    pub(super) observer: JoinHandle<()>,
}

impl Drop for LiveSession {
    fn drop(&mut self) {
        self.observer.abort();
    }
}

pub(super) enum SessionUpdate {
    Step {
        id: UiSessionId,
        step: Arc<StepEvent>,
    },
    Reset {
        id: UiSessionId,
        snapshot: Snapshot,
    },
    Closed {
        id: UiSessionId,
    },
}

pub(super) async fn observe_session(
    id: UiSessionId,
    agent: AgentHandle,
    observation: Observation,
    updates: mpsc::Sender<SessionUpdate>,
) {
    let Observation {
        snapshot: _,
        mut sequence,
        mut events,
    } = observation;
    loop {
        match events.recv().await {
            Ok(step) if step.sequence == sequence + 1 => {
                sequence = step.sequence;
                if updates
                    .send(SessionUpdate::Step { id, step })
                    .await
                    .is_err()
                {
                    return;
                }
            }
            Ok(_) | Err(RecvError::Lagged(_)) => match agent.observe().await {
                Ok(fresh) => {
                    let Observation {
                        snapshot,
                        sequence: fresh_sequence,
                        events: fresh_events,
                    } = fresh;
                    if updates
                        .send(SessionUpdate::Reset { id, snapshot })
                        .await
                        .is_err()
                    {
                        return;
                    }
                    sequence = fresh_sequence;
                    events = fresh_events;
                }
                Err(_) => {
                    let _ = updates.send(SessionUpdate::Closed { id }).await;
                    return;
                }
            },
            Err(RecvError::Closed) => {
                let _ = updates.send(SessionUpdate::Closed { id }).await;
                return;
            }
        }
    }
}

pub(super) fn enqueue_connection(
    connecting: &mut FuturesUnordered<ConnectionTask>,
    credentials: ChatGptCredentials,
    login_tx: mpsc::UnboundedSender<DeviceCodePrompt>,
) {
    connecting.push(tokio::spawn(async move {
        let auth = credentials.acquire().map_err(|error| error.to_string())?;
        let endpoint = chatgpt_subscription::connect("bone-agent", auth, move |prompt| {
            let _ = login_tx.send(prompt);
        })
        .await
        .map_err(|error| error.to_string())?;
        Ok(AgentHost::new(endpoint))
    }));
}

fn enqueue_start(
    starting: &mut FuturesUnordered<StartTask>,
    host: AgentHost,
    workspace: PathBuf,
    id: UiSessionId,
    runtime: ResolvedAgentRuntimeConfig,
) {
    starting.push(tokio::spawn(async move {
        let opened = match host.start(workspace, runtime) {
            Ok(agent) => match agent.observe().await {
                Ok(observation) => Ok((agent, observation)),
                Err(error) => Err(error.to_string()),
            },
            Err(error) => Err(error.to_string()),
        };
        (id, opened)
    }));
}

/// Start every accepted-but-not-yet-delivered turn exactly once for the
/// current connection attempt. Failed starts stay in `pending_tasks` and are
/// retried only after the user explicitly invokes `/login` again.
pub(super) fn enqueue_pending_starts(
    application: &WorkspaceApplication,
    durable: &mut HashMap<UiSessionId, DurableUiSession>,
    starting: &mut FuturesUnordered<StartTask>,
    starting_ids: &mut HashSet<UiSessionId>,
    host: &AgentHost,
    workspace: &Path,
    pending_tasks: &HashMap<UiSessionId, PendingRuntimeTurn>,
) -> EnqueuedPendingStarts {
    let mut ids = Vec::new();
    let mut notices = Vec::new();
    for (id, pending) in pending_tasks {
        if starting_ids.insert(*id) {
            ids.push(*id);
            if let Err(error) = persist_runtime_starting(application, durable, *id) {
                notices.push(format!(
                    "Saved message is starting, but its durable status could not be updated: {error}"
                ));
            }
            enqueue_start(
                starting,
                host.clone(),
                workspace.to_path_buf(),
                *id,
                pending.runtime.clone(),
            );
        }
    }
    EnqueuedPendingStarts { ids, notices }
}

/// A scheduled start is an effect result, not a direct presentation mutation.
/// Replaying it through the reducer restores the opening state after a failed
/// connection without dropping the already durable pending text.
pub(super) fn apply_enqueued_pending_starts(app: &mut App, started: EnqueuedPendingStarts) {
    for id in started.ids {
        if let Some(text) = app.pending_post(id).map(str::to_owned) {
            let _ = app.reduce(AppEvent::RuntimeStartQueued { id, text });
        }
    }
    for notice in started.notices {
        report_notice(app, notice);
    }
}

pub(super) fn persist_pending_retryable_statuses(
    application: &WorkspaceApplication,
    durable: &mut HashMap<UiSessionId, DurableUiSession>,
    app: &mut App,
    pending_tasks: &HashMap<UiSessionId, PendingRuntimeTurn>,
) {
    for id in pending_tasks.keys().copied().collect::<Vec<_>>() {
        if let Err(error) = persist_runtime_retryable(application, durable, id) {
            report_notice(
                app,
                format!(
                    "Saved message is retryable, but its durable status could not be updated: {error}"
                ),
            );
        }
    }
}

/// Sessions that still have a process-local runtime concern retain their
/// writer lease when the user switches away. The controller deliberately sees
/// only this compact ownership set, not runtime handles or task state.
pub(super) fn busy_session_ids(
    live_sessions: &[LiveSession],
    pending_tasks: &HashMap<UiSessionId, PendingRuntimeTurn>,
    starting_ids: &HashSet<UiSessionId>,
) -> HashSet<UiSessionId> {
    live_sessions
        .iter()
        .map(|session| session.id)
        .chain(pending_tasks.keys().copied())
        .chain(starting_ids.iter().copied())
        .collect()
}
