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

use crate::{LlmProfile, ProviderConnector, ResolvedRuntime, WorkspaceApplication};
use bone_agent::{AgentHandle, Observation, Snapshot, StepEvent};
use bone_llm::service::chatgpt_subscription::DeviceCodePrompt;
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

pub(super) type StartTask = JoinHandle<(UiSessionId, Result<(AgentHandle, Observation), String>)>;
pub(super) type AuthenticationTask = JoinHandle<Result<(), String>>;

/// Immutable dependencies shared by runtime-start effects in one TUI process.
/// Keeping these together leaves the scheduling function responsible only for
/// the mutable session/task collections it actually changes.
pub(super) struct RuntimeStartContext<'a> {
    pub(super) application: &'a WorkspaceApplication,
    pub(super) connector: &'a ProviderConnector,
    pub(super) login_tx: &'a mpsc::UnboundedSender<DeviceCodePrompt>,
    pub(super) workspace: &'a Path,
}

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

fn enqueue_start(
    starting: &mut FuturesUnordered<StartTask>,
    connector: ProviderConnector,
    login_tx: mpsc::UnboundedSender<DeviceCodePrompt>,
    workspace: PathBuf,
    id: UiSessionId,
    runtime: ResolvedRuntime,
) {
    starting.push(tokio::spawn(async move {
        let host = connector
            .connect_agent(&runtime, move |prompt| {
                let _ = login_tx.send(prompt);
            })
            .await;
        let opened = match host {
            Ok(host) => match host.start(workspace, runtime.agent.clone()) {
                Ok(agent) => match agent.observe().await {
                    Ok(observation) => Ok((agent, observation)),
                    Err(error) => Err(error.to_string()),
                },
                Err(error) => Err(error.to_string()),
            },
            Err(error) => Err(error.to_string()),
        };
        (id, opened)
    }));
}

/// Begin the explicit ChatGPT login flow without attaching an Agent runtime.
/// The UI receives device-code prompts through its existing event channel.
pub(super) fn start_chatgpt_authentication(
    connector: ProviderConnector,
    profile: LlmProfile,
    login_tx: mpsc::UnboundedSender<DeviceCodePrompt>,
) -> AuthenticationTask {
    tokio::spawn(async move {
        connector
            .authenticate_chatgpt(&profile, move |prompt| {
                let _ = login_tx.send(prompt);
            })
            .await
            .map_err(|error| error.to_string())
    })
}

/// Start one accepted-but-not-yet-delivered turn exactly once. A task resolves
/// its own frozen profile plan, so sessions may use independent providers;
/// failures remain retryable until that conversation's user invokes `/login`.
/// It deliberately accepts one ID rather than scanning every pending task:
/// one conversation must never cause another conversation's text to be sent.
pub(super) fn enqueue_pending_start(
    context: &RuntimeStartContext<'_>,
    durable: &mut HashMap<UiSessionId, DurableUiSession>,
    starting: &mut FuturesUnordered<StartTask>,
    starting_ids: &mut HashSet<UiSessionId>,
    id: UiSessionId,
    pending: &PendingRuntimeTurn,
) -> EnqueuedPendingStarts {
    let mut ids = Vec::new();
    let mut notices = Vec::new();
    if starting_ids.insert(id) {
        ids.push(id);
        if let Err(error) = persist_runtime_starting(context.application, durable, id) {
            notices.push(format!(
                "Saved message is starting, but its durable status could not be updated: {error}"
            ));
        }
        enqueue_start(
            starting,
            context.connector.clone(),
            context.login_tx.clone(),
            context.workspace.to_path_buf(),
            id,
            pending.runtime.clone(),
        );
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
