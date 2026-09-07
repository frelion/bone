//! Full-screen terminal frontend and event export for the shared Agent API.

#![forbid(unsafe_code)]

mod app;
mod config;
mod events;
mod terminal;
mod view;

use std::{io, path::PathBuf, sync::Arc};

use app::{Action, App, SessionId};
use bone_agent::{
    AgentHandle, AgentHost, HandleError, Observation, ShutdownReport, Snapshot, StartError,
    StepEvent, TaskConfig,
};
use crossterm::event::EventStream;
use futures_util::{StreamExt, future::join_all, stream::FuturesUnordered};
use terminal::TerminalSession;
use tokio::{
    sync::{broadcast::error::RecvError, mpsc},
    task::JoinHandle,
};

pub use config::TuiConfig;
pub use events::write_events;

#[derive(Debug, thiserror::Error)]
pub enum TuiError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Agent(#[from] HandleError),
    #[error(transparent)]
    Start(#[from] StartError),
}

/// Run a full-screen workspace with independent, concurrently active sessions.
/// The screen is restored before all sessions are shut down concurrently.
pub async fn run(
    host: &AgentHost,
    workspace: PathBuf,
    task: TaskConfig,
    config: &TuiConfig,
) -> Result<Vec<ShutdownReport>, TuiError> {
    let first = host.start(&workspace, task.clone()).await?;
    let observation = first.observe().await?;
    let (updates, update_rx) = mpsc::channel(256);
    let id = SessionId(1);
    let mut app = App::new(workspace.display().to_string());
    app.add_session(id, &observation.snapshot, config.show_progress);
    let observer = tokio::spawn(observe_session(
        id,
        first.clone(),
        observation,
        updates.clone(),
    ));
    let mut sessions = vec![LiveSession {
        id,
        agent: first,
        observer,
    }];

    let ui_result: Result<(), TuiError> = async {
        let mut update_rx = update_rx;
        let mut terminal = TerminalSession::enter()?;
        let mut input = EventStream::new();
        let mut starting = FuturesUnordered::new();

        loop {
            let mut viewport = Default::default();
            terminal.draw(|frame| viewport = view::render(frame, &app))?;
            app.set_viewport(viewport);

            tokio::select! {
                event = input.next() => {
                    let Some(event) = event else { return Ok(()); };
                    match app.on_event(event?) {
                        Action::None => {}
                        Action::Post { id, text } => {
                            match session_handle(&sessions, id).post(text).await {
                                Ok(_) => app.clear_composer(id),
                                Err(error) => app.mark_offline(id, format!("send failed: {error}")),
                            }
                        }
                        Action::Stop { id, clear } => {
                            match session_handle(&sessions, id).stop().await {
                                Ok(()) if clear => app.clear_composer(id),
                                Ok(()) => {}
                                Err(error) => app.mark_offline(id, format!("stop failed: {error}")),
                            }
                        }
                        Action::NewSession => {
                            let id = SessionId(app.sessions.len() as u64 + 1);
                            app.begin_session(id, config.show_progress);
                            let host = (*host).clone();
                            let workspace = workspace.clone();
                            let task = task.clone();
                            starting.push(async move {
                                let opened = match host.start(workspace, task).await {
                                    Ok(agent) => match agent.observe().await {
                                        Ok(observation) => Ok((agent, observation)),
                                        Err(error) => Err(error.to_string()),
                                    },
                                    Err(error) => Err(error.to_string()),
                                };
                                (id, opened)
                            });
                        }
                        Action::Quit => return Ok(()),
                    }
                }
                opened = starting.next(), if !starting.is_empty() => {
                    let (id, opened) = opened.expect("a pending session start exists");
                    match opened {
                        Ok((agent, observation)) => {
                            let pending = app.attach(id, &observation.snapshot);
                            let observer = tokio::spawn(observe_session(
                                id,
                                agent.clone(),
                                observation,
                                updates.clone(),
                            ));
                            if let Some(text) = pending {
                                match agent.post(text).await {
                                    Ok(_) => app.acknowledge_pending_post(id),
                                    Err(error) => {
                                        app.restore_pending_post(id);
                                        app.mark_offline(id, format!("send failed: {error}"));
                                    }
                                }
                            }
                            sessions.push(LiveSession { id, agent, observer });
                        }
                        Err(error) => {
                            app.restore_pending_post(id);
                            app.mark_offline(id, format!("could not open: {error}"));
                        }
                    }
                }
                update = update_rx.recv() => match update {
                    Some(SessionUpdate::Step { id, step }) => app.apply(id, &step),
                    Some(SessionUpdate::Reset { id, snapshot }) => app.reset(id, &snapshot),
                    Some(SessionUpdate::Closed { id }) => {
                        app.mark_offline(id, "agent runtime closed")
                    }
                    None => return Ok(()),
                }
            }
        }
    }
    .await;
    drop(updates);

    let reports = join_all(sessions.iter().map(|session| session.agent.shutdown())).await;
    for session in &mut sessions {
        let _ = (&mut session.observer).await;
    }

    ui_result?;
    reports
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn session_handle(sessions: &[LiveSession], id: SessionId) -> AgentHandle {
    sessions
        .iter()
        .find(|session| session.id == id)
        .expect("UI action belongs to an existing session")
        .agent
        .clone()
}

struct LiveSession {
    id: SessionId,
    agent: AgentHandle,
    observer: JoinHandle<()>,
}

impl Drop for LiveSession {
    fn drop(&mut self) {
        self.observer.abort();
    }
}

enum SessionUpdate {
    Step { id: SessionId, step: Arc<StepEvent> },
    Reset { id: SessionId, snapshot: Snapshot },
    Closed { id: SessionId },
}

async fn observe_session(
    id: SessionId,
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

#[cfg(test)]
mod tests {
    use std::{
        future::Future,
        pin::Pin,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use bone_agent::{
        Autonomy, JobContext, JobOutcome, ModelInput, ModelPort, Next, Notice, Runtime, WorkResult,
    };

    use super::{SessionId, SessionUpdate, observe_session};

    struct BusyModel(AtomicUsize);

    impl ModelPort for BusyModel {
        fn infer(
            &self,
            _: ModelInput,
            _: JobContext,
        ) -> Pin<Box<dyn Future<Output = JobOutcome> + Send>> {
            let call = self.0.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                JobOutcome::work(WorkResult {
                    autonomy: Autonomy::Run,
                    next: if call == 300 {
                        Next::Finish
                    } else {
                        Next::Continue
                    },
                    ..Default::default()
                })
            })
        }
    }

    #[tokio::test]
    async fn a_lagging_session_observer_resets_only_its_tagged_session() {
        let agent = Runtime::spawn(
            Arc::new(BusyModel(AtomicUsize::new(0))),
            vec![],
            Default::default(),
            Default::default(),
        )
        .unwrap();
        let observation = agent.observe().await.unwrap();
        let mut notices = agent.subscribe();
        let (updates, mut update_rx) = tokio::sync::mpsc::channel(1);
        let observer = tokio::spawn(observe_session(
            SessionId(7),
            agent.clone(),
            observation,
            updates,
        ));

        agent.post("keep reasoning").await.unwrap();
        loop {
            if matches!(notices.recv().await.unwrap(), Notice::Finished { .. }) {
                break;
            }
        }

        let reset = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some(SessionUpdate::Reset { id, snapshot }) = update_rx.recv().await {
                    break (id, snapshot);
                }
            }
        })
        .await
        .expect("the observer should recover from its broadcast gap");
        assert_eq!(reset.0, SessionId(7));
        assert!(reset.1.record.iter().any(|entry| {
            matches!(
                &entry.kind,
                bone_agent::RecordKind::Notice(Notice::Finished { .. })
            )
        }));

        drop(update_rx);
        agent.shutdown().await.unwrap();
        observer.await.unwrap();
    }
}
