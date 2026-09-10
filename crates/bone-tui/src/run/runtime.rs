use std::{collections::BTreeMap, time::Duration};

use bone_app::{App, CreateSessionRequest, Session, SessionId};
use tokio::{sync::mpsc, task::AbortHandle};

use crate::state::{Effect, OperationKind, UiEvent, UiState};

use super::{RunError, summarize_overview};

const HISTORY_PAGE: usize = 32;
const DRAFT_DEBOUNCE: Duration = Duration::from_millis(250);

pub(super) struct Runtime {
    app: App,
    workspace: bone_app::WorkspaceId,
    sessions: BTreeMap<SessionId, Session>,
    observers: BTreeMap<SessionId, AbortHandle>,
    draft_saves: BTreeMap<SessionId, AbortHandle>,
    tx: mpsc::Sender<UiEvent>,
    ready_tx: mpsc::Sender<SessionReady>,
}

pub(super) enum SessionReady {
    Opened {
        id: SessionId,
        generation: u64,
        session: Session,
        views: tokio::sync::watch::Receiver<std::sync::Arc<bone_app::SessionView>>,
        snapshot: std::sync::Arc<bone_app::SessionView>,
        history: bone_app::RecentHistoryPage,
    },
    Created {
        request_id: bone_app::RequestId,
        session: Session,
        info: bone_app::SessionInfo,
    },
}

impl Runtime {
    pub(super) fn new(
        app: App,
        workspace: bone_app::WorkspaceId,
        tx: mpsc::Sender<UiEvent>,
        ready_tx: mpsc::Sender<SessionReady>,
    ) -> Self {
        Self {
            app,
            workspace,
            sessions: BTreeMap::new(),
            observers: BTreeMap::new(),
            draft_saves: BTreeMap::new(),
            tx,
            ready_tx,
        }
    }

    pub(super) async fn apply(&mut self, effect: Effect) -> Result<bool, RunError> {
        match effect {
            Effect::OpenSession {
                session,
                generation,
            } => {
                let app = self.app.clone();
                let ready_tx = self.ready_tx.clone();
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let opened = async {
                        let handle = app.session(session).await?;
                        let mut views = handle.observe();
                        let _ = handle.snapshot().await?;
                        let history = handle.recent_history(None, HISTORY_PAGE).await?;
                        let snapshot = views.borrow_and_update().clone();
                        Ok::<_, bone_app::Error>((handle, views, snapshot, history))
                    }
                    .await;
                    match opened {
                        Ok((handle, views, snapshot, history)) => {
                            let _ = ready_tx
                                .send(SessionReady::Opened {
                                    id: session,
                                    generation,
                                    session: handle,
                                    views,
                                    snapshot,
                                    history,
                                })
                                .await;
                        }
                        Err(_) => {
                            send_failure(
                                &tx,
                                OperationKind::OpenSession,
                                Some(session),
                                Some(generation),
                                "Unable to open the session".into(),
                            )
                            .await
                        }
                    }
                });
            }
            Effect::CreateSession {
                request_id,
                title,
                provisional,
            } => {
                let app = self.app.clone();
                let workspace = self.workspace;
                let ready_tx = self.ready_tx.clone();
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let request = CreateSessionRequest {
                        request_id,
                        workspace,
                        title,
                        provisional,
                    };
                    match app.create_session_idempotent(request).await {
                        Ok(session) => match session.snapshot().await {
                            Ok(snapshot) => {
                                let _ = ready_tx
                                    .send(SessionReady::Created {
                                        request_id,
                                        session,
                                        info: snapshot.session,
                                    })
                                    .await;
                            }
                            Err(_) => {
                                let _ = tx.send(UiEvent::SessionCreateFailed {
                                        request_id,
                                        message: "The session was created but could not be opened; press Enter to retry".into(),
                                    }).await;
                            }
                        },
                        Err(_) => {
                            let _ = tx
                                .send(UiEvent::SessionCreateFailed {
                                    request_id,
                                    message: "Unable to create the session; press Enter to retry"
                                        .into(),
                                })
                                .await;
                        }
                    }
                });
            }
            Effect::RenameSession {
                session,
                generation,
                title,
            } => {
                let handle = self.sessions.get(&session).cloned();
                let app = self.app.clone();
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let result = async {
                        let handle = match handle {
                            Some(value) => value,
                            None => app.session(session).await?,
                        };
                        handle.rename(title.clone()).await?;
                        Ok::<_, bone_app::Error>(title)
                    }
                    .await;
                    match result {
                        Ok(title) => {
                            let _ = tx
                                .send(UiEvent::SessionRenamed {
                                    session,
                                    generation,
                                    title,
                                })
                                .await;
                        }
                        Err(_) => {
                            send_failure(
                                &tx,
                                OperationKind::RenameSession,
                                Some(session),
                                Some(generation),
                                "Unable to rename the session".into(),
                            )
                            .await
                        }
                    }
                });
            }
            Effect::AutoTitle {
                session,
                generation,
                first_input,
            } => {
                let handle = self.sessions.get(&session).cloned();
                let app = self.app.clone();
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let result = async {
                        let handle = match handle {
                            Some(value) => value,
                            None => app.session(session).await?,
                        };
                        if handle.title_from_first_input(&first_input).await? {
                            Ok::<_, bone_app::Error>(Some(handle.snapshot().await?.session.title))
                        } else {
                            Ok(None)
                        }
                    }
                    .await;
                    match result {
                        Ok(Some(title)) => {
                            let _ = tx
                                .send(UiEvent::SessionRenamed {
                                    session,
                                    generation,
                                    title,
                                })
                                .await;
                        }
                        Ok(None) => {}
                        Err(_) => {
                            send_failure(
                                &tx,
                                OperationKind::AutoTitle,
                                Some(session),
                                Some(generation),
                                "The session title could not be updated".into(),
                            )
                            .await
                        }
                    }
                });
            }
            Effect::SaveDraft {
                session,
                generation,
                revision,
                text,
            } => {
                if let Some(previous) = self.draft_saves.remove(&session) {
                    previous.abort();
                }
                let handle = self.sessions.get(&session).cloned();
                let app = self.app.clone();
                let tx = self.tx.clone();
                let task = tokio::spawn(async move {
                    tokio::time::sleep(DRAFT_DEBOUNCE).await;
                    let saved = async {
                        let handle = match handle {
                            Some(value) => value,
                            None => app.session(session).await?,
                        };
                        handle.save_draft(text.clone()).await?;
                        Ok::<_, bone_app::Error>(text)
                    }
                    .await;
                    match saved {
                        Ok(text) => {
                            let _ = tx
                                .send(UiEvent::DraftSaved {
                                    session,
                                    generation,
                                    revision,
                                    text,
                                })
                                .await;
                        }
                        Err(_) => {
                            send_failure(
                                &tx,
                                OperationKind::SaveDraft,
                                Some(session),
                                Some(generation),
                                "Your draft could not be saved".into(),
                            )
                            .await
                        }
                    }
                });
                self.draft_saves.insert(session, task.abort_handle());
            }
            Effect::Submit {
                session,
                generation,
                input,
            } => {
                let handle = self.sessions.get(&session).cloned();
                let app = self.app.clone();
                let tx = self.tx.clone();
                let request_id = input.request_id;
                tokio::spawn(async move {
                    let submitted = async {
                        let handle = match handle {
                            Some(handle) => handle,
                            None => app.session(session).await?,
                        };
                        handle.submit(input).await
                    }
                    .await;
                    match submitted {
                        Ok(receipt) => {
                            let _ = tx
                                .send(UiEvent::Submitted {
                                    session,
                                    generation,
                                    request_id,
                                    receipt,
                                })
                                .await;
                        }
                        Err(_) => {
                            let _ = tx
                                .send(UiEvent::SubmitFailed {
                                    session,
                                    generation,
                                    request_id,
                                    message: "Your request was not sent; the draft has been kept"
                                        .into(),
                                })
                                .await;
                        }
                    }
                });
            }
            Effect::Stop {
                session,
                generation,
            } => {
                if let Some(handle) = self.sessions.get(&session).cloned() {
                    let tx = self.tx.clone();
                    tokio::spawn(async move {
                        if handle.stop().await.is_err() {
                            send_failure(
                                &tx,
                                OperationKind::Stop,
                                Some(session),
                                Some(generation),
                                "Unable to stop the current work".into(),
                            )
                            .await;
                        }
                    });
                }
            }
            Effect::LoadHistory {
                session,
                generation,
                after,
            } => {
                if let Some(handle) = self.sessions.get(&session).cloned() {
                    let tx = self.tx.clone();
                    tokio::spawn(async move {
                        match handle.history(after, HISTORY_PAGE).await {
                            Ok(page) => {
                                let _ = tx
                                    .send(UiEvent::HistoryLoaded {
                                        session,
                                        generation,
                                        page,
                                    })
                                    .await;
                            }
                            Err(_) => {
                                send_failure(
                                    &tx,
                                    OperationKind::LoadHistory,
                                    Some(session),
                                    Some(generation),
                                    "Unable to load recent activity".into(),
                                )
                                .await
                            }
                        }
                    });
                }
            }
            Effect::LoadOlderHistory {
                session,
                generation,
                cursor,
            } => {
                if let Some(handle) = self.sessions.get(&session).cloned() {
                    let tx = self.tx.clone();
                    tokio::spawn(async move {
                        match handle.recent_history(Some(cursor), HISTORY_PAGE).await {
                            Ok(page) => {
                                let _ = tx
                                    .send(UiEvent::OlderHistoryLoaded {
                                        session,
                                        generation,
                                        page,
                                    })
                                    .await;
                            }
                            Err(_) => {
                                send_failure(
                                    &tx,
                                    OperationKind::LoadOlderHistory,
                                    Some(session),
                                    Some(generation),
                                    "Unable to load older activity".into(),
                                )
                                .await
                            }
                        }
                    });
                }
            }
            Effect::ReloadRecentHistory {
                session,
                generation,
            } => {
                if let Some(handle) = self.sessions.get(&session).cloned() {
                    let tx = self.tx.clone();
                    tokio::spawn(async move {
                        match handle.recent_history(None, HISTORY_PAGE).await {
                            Ok(page) => {
                                let _ = tx
                                    .send(UiEvent::RecentHistoryReloaded {
                                        session,
                                        generation,
                                        page,
                                    })
                                    .await;
                            }
                            Err(_) => {
                                send_failure(
                                    &tx,
                                    OperationKind::LoadHistory,
                                    Some(session),
                                    Some(generation),
                                    "Unable to return to recent activity".into(),
                                )
                                .await
                            }
                        }
                    });
                }
            }
            Effect::RefreshOverview { generation } => {
                let app = self.app.clone();
                let workspace = self.workspace;
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    match app.workspace_overview(workspace).await {
                        Ok(overview) => {
                            let (sessions, statuses) = summarize_overview(&overview);
                            let _ = tx
                                .send(UiEvent::OverviewLoaded {
                                    generation,
                                    sessions,
                                    statuses,
                                })
                                .await;
                        }
                        Err(_) => {
                            send_failure(
                                &tx,
                                OperationKind::RefreshOverview,
                                None,
                                None,
                                "Unable to refresh the workspace".into(),
                            )
                            .await
                        }
                    }
                });
            }
            Effect::ReleaseSession {
                session,
                generation,
            } => {
                let app = self.app.clone();
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    match app.release_session(session).await {
                        Ok(receipt) => {
                            let _ = tx
                                .send(UiEvent::SessionReleased {
                                    generation,
                                    receipt,
                                })
                                .await;
                        }
                        Err(_) => {
                            send_failure(
                                &tx,
                                OperationKind::ReleaseSession,
                                Some(session),
                                Some(generation),
                                "Unable to release the session".into(),
                            )
                            .await
                        }
                    }
                });
            }
            Effect::RememberSession { workspace, session } => {
                if self
                    .app
                    .set_last_active_session(workspace, session)
                    .await
                    .is_err()
                {
                    let _ = self
                        .tx
                        .send(UiEvent::OperationFailed {
                            kind: OperationKind::RememberSession,
                            session: Some(session),
                            generation: None,
                            message: "Unable to remember the active session".into(),
                        })
                        .await;
                }
            }
            Effect::Shutdown => return Ok(true),
        }
        Ok(false)
    }

    fn observe(
        &mut self,
        id: SessionId,
        generation: u64,
        mut views: tokio::sync::watch::Receiver<std::sync::Arc<bone_app::SessionView>>,
    ) {
        if let Some(previous) = self.observers.remove(&id) {
            previous.abort();
        }
        let tx = self.tx.clone();
        let task = tokio::spawn(async move {
            while views.changed().await.is_ok() {
                let snapshot = views.borrow_and_update().clone();
                if tx
                    .send(UiEvent::SessionChanged {
                        session: id,
                        generation,
                        snapshot,
                    })
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });
        self.observers.insert(id, task.abort_handle());
    }

    pub(super) fn accept_ready(&mut self, ready: SessionReady, state: &UiState) -> Option<UiEvent> {
        match ready {
            SessionReady::Opened {
                id,
                generation,
                session,
                views,
                snapshot,
                history,
            } => {
                if !state
                    .session_ui
                    .get(&id)
                    .is_some_and(|ui| ui.generation == generation)
                {
                    // A newer open for the same selected session may already be in flight.
                    // Releasing by id here would tear down that newer lease as well.
                    if state.selected != Some(id) {
                        let app = self.app.clone();
                        tokio::spawn(async move {
                            let _ = app.release_session(id).await;
                        });
                    }
                    return None;
                }
                self.sessions.insert(id, session);
                self.observe(id, generation, views);
                Some(UiEvent::SessionOpened {
                    session: id,
                    generation,
                    snapshot,
                    history,
                })
            }
            SessionReady::Created {
                request_id,
                session,
                info,
            } => {
                if !state
                    .pending_create
                    .as_ref()
                    .is_some_and(|pending| pending.request_id == request_id)
                {
                    let app = self.app.clone();
                    let id = info.id;
                    tokio::spawn(async move {
                        let _ = app.release_session(id).await;
                    });
                    return None;
                }
                self.sessions.insert(info.id, session);
                Some(UiEvent::SessionCreated { request_id, info })
            }
        }
    }

    pub(super) fn accept_event(&mut self, event: &UiEvent, _state: &UiState) {
        if let UiEvent::SessionReleased { receipt, .. } = event
            && matches!(
                receipt.status,
                bone_app::SessionReleaseStatus::Released | bone_app::SessionReleaseStatus::NotOpen
            )
        {
            self.sessions.remove(&receipt.session);
            if let Some(task) = self.observers.remove(&receipt.session) {
                task.abort();
            }
            if let Some(task) = self.draft_saves.remove(&receipt.session) {
                task.abort();
            }
        }
    }

    pub(super) async fn flush_drafts(&mut self, state: &UiState) -> Result<(), bone_app::Error> {
        for task in self.draft_saves.values() {
            task.abort();
        }
        self.draft_saves.clear();
        let mut first_error = None;
        for (id, ui) in &state.session_ui {
            if ui.draft_revision > ui.saved_draft_revision {
                let session = match self.sessions.get(id).cloned() {
                    Some(session) => Ok(session),
                    None => self.app.session(*id).await,
                };
                match session {
                    Ok(session) => {
                        if let Err(error) = session.save_draft(ui.draft.clone()).await {
                            first_error.get_or_insert(error);
                        }
                    }
                    Err(error) => {
                        first_error.get_or_insert(error);
                    }
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    pub(super) async fn shutdown(self) -> Result<(), bone_app::Error> {
        self.app.shutdown().await.map(|_| ())
    }
}

async fn send_failure(
    tx: &mpsc::Sender<UiEvent>,
    kind: OperationKind,
    session: Option<SessionId>,
    generation: Option<u64>,
    message: String,
) {
    let _ = tx
        .send(UiEvent::OperationFailed {
            kind,
            session,
            generation,
            message,
        })
        .await;
}
