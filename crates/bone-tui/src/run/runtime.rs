use std::{collections::BTreeMap, sync::Arc, time::Duration};

use bone_app::{App, CreateSessionRequest, Session, SessionId};
use futures_util::{FutureExt, future::Shared};
use tokio::{sync::mpsc, task::AbortHandle};

use crate::state::{Effect, OperationKind, UiEvent, UiState};

use super::summarize_overview;

const HISTORY_PAGE: usize = 32;
const DRAFT_DEBOUNCE: Duration = Duration::from_millis(250);

#[derive(Clone, Debug)]
enum SessionOperationOutcome {
    Renamed(Result<String, Arc<str>>),
    AutoTitled(Result<Option<String>, Arc<str>>),
    Released(Result<bone_app::SessionReleaseReceipt, Arc<str>>),
}

type SharedSessionOperation =
    Shared<futures_util::future::BoxFuture<'static, SessionOperationOutcome>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SessionOperationToken {
    Rename(u64),
    AutoTitle(u64),
    Release(u64),
}

struct PendingSessionOperation {
    token: SessionOperationToken,
    future: SharedSessionOperation,
}

#[derive(Default)]
struct SessionResources {
    handle: Option<Session>,
    observer: Option<AbortHandle>,
    draft_save: Option<AbortHandle>,
}

pub(super) struct Runtime {
    login: Option<AbortHandle>,
    app: App,
    workspace: bone_app::WorkspaceId,
    sessions: BTreeMap<SessionId, SessionResources>,
    session_operations: BTreeMap<SessionId, PendingSessionOperation>,
    // Kept across failed or timed-out exit flushes, including late create receipts.
    exit_draft_request: Option<CreateSessionRequest>,
    tx: mpsc::Sender<UiEvent>,
    ready_tx: mpsc::Sender<SessionReady>,
}

pub(super) enum SessionReady {
    Opened {
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
            login: None,
            app,
            workspace,
            sessions: BTreeMap::new(),
            session_operations: BTreeMap::new(),
            exit_draft_request: None,
            tx,
            ready_tx,
        }
    }

    pub(super) async fn apply(&mut self, effect: Effect) -> bool {
        match effect {
            Effect::SaveConnection {
                request,
                session,
                profile,
                key,
                selection,
            } => {
                let app = self.app.clone();
                let workspace = self.workspace;
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let result = async {
                        let key = key
                            .map(|mut key| bone_app::ApiKey::new(key.take()))
                            .transpose()
                            .map_err(|_| "Invalid API key; nothing was saved")?;
                        super::models::save_connection(
                            &app, workspace, session, profile, key, selection,
                        )
                        .await
                    }
                    .await;
                    let _ = tx
                        .send(UiEvent::ConnectionSaved {
                            request,
                            session,
                            notice: result.as_ref().ok().copied().flatten().map(str::to_owned),
                            error: result.err().map(str::to_owned),
                        })
                        .await;
                });
            }
            Effect::CancelLogin => {
                if let Some(login) = self.login.take() {
                    login.abort();
                }
            }
            Effect::Login { profile, request } => {
                if let Some(login) = self.login.take() {
                    login.abort();
                }
                let app = self.app.clone();
                let tx = self.tx.clone();
                self.login = Some(
                    tokio::spawn(async move {
                        match app.login(profile).await {
                            Ok(attempt) => {
                                let mut changes = attempt.observe();
                                loop {
                                    let state = changes.borrow_and_update().as_ref().clone();
                                    let done = matches!(
                                        state,
                                        bone_app::LoginState::Succeeded
                                            | bone_app::LoginState::Failed { .. }
                                            | bone_app::LoginState::Cancelled
                                    );
                                    if tx
                                        .send(UiEvent::LoginChanged { request, state })
                                        .await
                                        .is_err()
                                        || done
                                    {
                                        break;
                                    }
                                    if changes.changed().await.is_err() {
                                        break;
                                    }
                                }
                            }
                            Err(error) => {
                                let _ = tx
                                    .send(UiEvent::LoginChanged {
                                        request,
                                        state: bone_app::LoginState::Failed {
                                            message: error.to_string(),
                                        },
                                    })
                                    .await;
                            }
                        }
                    })
                    .abort_handle(),
                );
            }

            Effect::LoadModelLabel { session, request } => {
                let app = self.app.clone();
                let workspace = self.workspace;
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let facts = super::models::facts(&app, workspace, session).await;
                    let label = facts
                        .as_ref()
                        .and_then(|facts| facts.saved.as_ref().ok())
                        .map(|model| model.selection.model.clone());
                    let _ = tx
                        .send(UiEvent::ModelLabelLoaded {
                            session,
                            request,
                            label,
                            facts,
                        })
                        .await;
                });
            }
            Effect::LoadModels { session, request } => {
                let app = self.app.clone();
                let workspace = self.workspace;
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let result = async {
                        Ok::<_, bone_app::Error>((
                            super::models::load(&app, workspace, session).await?,
                            super::models::profiles(&app).await?,
                        ))
                    }
                    .await;
                    let event = match result {
                        Ok((choices, profiles)) => UiEvent::ModelsLoaded {
                            session,
                            request,
                            choices,
                            profiles,
                        },
                        Err(error) => UiEvent::ModelsFailed {
                            session,
                            request,
                            error: error.to_string(),
                        },
                    };
                    let _ = tx.send(event).await;
                });
            }
            Effect::SetModel {
                session,
                request,
                selection,
            } => {
                let app = self.app.clone();
                let workspace = self.workspace;
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let choice = super::models::ModelChoice {
                        selection,
                        profile_label: String::new(),
                    };
                    let result = match session {
                        Some(session) => super::models::apply(&app, session, &choice).await,
                        None => {
                            app.update_config(
                                bone_app::ConfigScope::Workspace(workspace),
                                bone_app::ConfigChange::Worker(Some(choice.selection)),
                            )
                            .await
                        }
                    };
                    model_applied(&app, workspace, &tx, session, request, result.err()).await;
                });
            }
            Effect::SetNamedModel {
                session,
                request,
                profile,
                model,
            } => {
                let app = self.app.clone();
                let workspace = self.workspace;
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let result = async {
                        let choice = super::models::explicit(&app, &profile, &model).await?;
                        match session {
                            Some(session) => super::models::apply(&app, session, &choice).await,
                            None => {
                                app.update_config(
                                    bone_app::ConfigScope::Workspace(workspace),
                                    bone_app::ConfigChange::Worker(Some(choice.selection)),
                                )
                                .await
                            }
                        }
                    }
                    .await;
                    model_applied(&app, workspace, &tx, session, request, result.err()).await;
                });
            }

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
                request,
                title,
            } => {
                let app = self.app.clone();
                let previous = self
                    .session_operations
                    .get(&session)
                    .map(|operation| operation.future.clone());
                let future = async move {
                    if let Some(previous) = previous {
                        let _ = previous.await;
                    }
                    let result = async {
                        // Resolve after the per-Session barrier. A release ahead
                        // of this operation may have closed the cached actor.
                        let handle = app.session(session).await?;
                        handle.rename(title.clone()).await?;
                        Ok::<_, bone_app::Error>(title)
                    }
                    .await
                    .map_err(|error| Arc::<str>::from(error.to_string()));
                    SessionOperationOutcome::Renamed(result)
                }
                .boxed()
                .shared();
                self.session_operations.insert(
                    session,
                    PendingSessionOperation {
                        token: SessionOperationToken::Rename(request),
                        future: future.clone(),
                    },
                );
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    match future.await {
                        SessionOperationOutcome::Renamed(Ok(title)) => {
                            let _ = tx
                                .send(UiEvent::SessionRenamed {
                                    session,
                                    request,
                                    title,
                                })
                                .await;
                        }
                        SessionOperationOutcome::Renamed(Err(_)) => {
                            let _ = tx
                                .send(UiEvent::SessionRenameFailed {
                                    session,
                                    request,
                                    message: "Unable to rename the session".into(),
                                })
                                .await;
                        }
                        SessionOperationOutcome::AutoTitled(_)
                        | SessionOperationOutcome::Released(_) => {
                            unreachable!("manual title future returned an auto-title outcome")
                        }
                    }
                });
            }
            Effect::AutoTitle {
                session,
                generation,
                request,
                first_input,
            } => {
                let app = self.app.clone();
                let previous = self
                    .session_operations
                    .get(&session)
                    .map(|operation| operation.future.clone());
                let future = async move {
                    if let Some(previous) = previous {
                        let _ = previous.await;
                    }
                    let result = async {
                        let handle = app.session(session).await?;
                        if handle.title_from_first_input(&first_input).await? {
                            Ok::<_, bone_app::Error>(Some(handle.snapshot().await?.session.title))
                        } else {
                            Ok(None)
                        }
                    }
                    .await
                    .map_err(|error| Arc::<str>::from(error.to_string()));
                    SessionOperationOutcome::AutoTitled(result)
                }
                .boxed()
                .shared();
                self.session_operations.insert(
                    session,
                    PendingSessionOperation {
                        token: SessionOperationToken::AutoTitle(request),
                        future: future.clone(),
                    },
                );
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    match future.await {
                        SessionOperationOutcome::AutoTitled(Ok(Some(title))) => {
                            let _ = tx
                                .send(UiEvent::SessionAutoTitled {
                                    session,
                                    request,
                                    title,
                                })
                                .await;
                        }
                        SessionOperationOutcome::AutoTitled(Ok(None)) => {}
                        SessionOperationOutcome::AutoTitled(Err(_)) => {
                            send_failure(
                                &tx,
                                OperationKind::AutoTitle,
                                Some(session),
                                Some(generation),
                                "The session title could not be updated".into(),
                            )
                            .await
                        }
                        SessionOperationOutcome::Renamed(_)
                        | SessionOperationOutcome::Released(_) => {
                            unreachable!("auto-title future returned a manual title outcome")
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
                if let Some(previous) = self
                    .sessions
                    .get_mut(&session)
                    .and_then(|resources| resources.draft_save.take())
                {
                    previous.abort();
                }
                let handle = self.session(session);
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
                self.sessions.entry(session).or_default().draft_save = Some(task.abort_handle());
            }
            Effect::Submit {
                session,
                generation,
                input,
            } => {
                let handle = self.session(session);
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
            Effect::RetryInput {
                session,
                generation,
                input,
            } => {
                let handle = self.session(session);
                let app = self.app.clone();
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let retried = async {
                        let handle = match handle {
                            Some(handle) => handle,
                            None => app.session(session).await?,
                        };
                        handle.retry(input).await
                    }
                    .await;
                    if let Err(error) = retried {
                        send_failure(
                            &tx,
                            OperationKind::RetryInput,
                            Some(session),
                            Some(generation),
                            error.to_string(),
                        )
                        .await;
                    }
                });
            }
            Effect::Stop {
                session,
                generation,
            } => {
                if let Some(handle) = self.session(session) {
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
                if let Some(handle) = self.session(session) {
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
                if let Some(handle) = self.session(session) {
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
                if let Some(handle) = self.session(session) {
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
                            let (sessions, statuses, summaries) = summarize_overview(&overview);
                            let _ = tx
                                .send(UiEvent::OverviewLoaded {
                                    generation,
                                    sessions,
                                    statuses,
                                    summaries,
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
                let previous = self
                    .session_operations
                    .get(&session)
                    .map(|operation| operation.future.clone());
                let future = async move {
                    if let Some(previous) = previous {
                        let _ = previous.await;
                    }
                    SessionOperationOutcome::Released(
                        app.release_session(session)
                            .await
                            .map_err(|error| Arc::<str>::from(error.to_string())),
                    )
                }
                .boxed()
                .shared();
                self.session_operations.insert(
                    session,
                    PendingSessionOperation {
                        token: SessionOperationToken::Release(generation),
                        future: future.clone(),
                    },
                );
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    match future.await {
                        SessionOperationOutcome::Released(Ok(receipt)) => {
                            let _ = tx
                                .send(UiEvent::SessionReleased {
                                    generation,
                                    receipt,
                                })
                                .await;
                        }
                        SessionOperationOutcome::Released(Err(_)) => {
                            send_failure(
                                &tx,
                                OperationKind::ReleaseSession,
                                Some(session),
                                Some(generation),
                                "Unable to release the session".into(),
                            )
                            .await
                        }
                        SessionOperationOutcome::Renamed(_)
                        | SessionOperationOutcome::AutoTitled(_) => {
                            unreachable!("release future returned a title outcome")
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
            Effect::Shutdown => return true,
        }
        false
    }

    fn observe(
        &mut self,
        id: SessionId,
        generation: u64,
        mut views: tokio::sync::watch::Receiver<std::sync::Arc<bone_app::SessionView>>,
    ) {
        if let Some(previous) = self
            .sessions
            .get_mut(&id)
            .and_then(|resources| resources.observer.take())
        {
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
        self.sessions
            .get_mut(&id)
            .expect("a session handle is retained before observation")
            .observer = Some(task.abort_handle());
    }

    fn retain_session(&mut self, session: Session) {
        let id = session.id();
        self.sessions.entry(id).or_default().handle = Some(session);
    }

    fn session(&self, id: SessionId) -> Option<Session> {
        self.sessions
            .get(&id)
            .and_then(|resources| resources.handle.clone())
    }

    pub(super) fn accept_ready(&mut self, ready: SessionReady, state: &UiState) -> Option<UiEvent> {
        match ready {
            SessionReady::Opened {
                generation,
                session,
                views,
                snapshot,
                history,
            } => {
                let id = session.id();
                if state
                    .session_ui
                    .get(&id)
                    .is_none_or(|ui| ui.generation != generation)
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
                self.retain_session(session);
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
                if self
                    .exit_draft_request
                    .as_ref()
                    .is_some_and(|request| request.request_id == request_id)
                {
                    // A timed-out exit can receive the original create callback later.
                    // Keep the orphan editor and pending identity until its save succeeds.
                    self.retain_session(session);
                    return None;
                }
                if state
                    .pending_create
                    .as_ref()
                    .is_none_or(|pending| pending.request_id != request_id)
                {
                    let app = self.app.clone();
                    let id = info.id;
                    tokio::spawn(async move {
                        let _ = app.release_session(id).await;
                    });
                    return None;
                }
                self.retain_session(session);
                Some(UiEvent::SessionCreated { request_id, info })
            }
        }
    }

    pub(super) fn accept_event(&mut self, event: &UiEvent) {
        let completed_operation = match event {
            UiEvent::SessionRenamed {
                session, request, ..
            }
            | UiEvent::SessionRenameFailed {
                session, request, ..
            } => Some((*session, SessionOperationToken::Rename(*request))),
            UiEvent::SessionAutoTitled {
                session, request, ..
            } => Some((*session, SessionOperationToken::AutoTitle(*request))),
            UiEvent::SessionReleased {
                generation,
                receipt,
            } => Some((receipt.session, SessionOperationToken::Release(*generation))),
            UiEvent::OperationFailed {
                kind: OperationKind::ReleaseSession,
                session: Some(session),
                generation: Some(generation),
                ..
            } => Some((*session, SessionOperationToken::Release(*generation))),
            _ => None,
        };
        if let Some((session, token)) = completed_operation
            && self
                .session_operations
                .get(&session)
                .is_some_and(|operation| operation.token == token)
        {
            self.session_operations.remove(&session);
        }
        if let UiEvent::SessionReleased { receipt, .. } = event
            && matches!(
                receipt.status,
                bone_app::SessionReleaseStatus::Released | bone_app::SessionReleaseStatus::NotOpen
            )
            && let Some(resources) = self.sessions.remove(&receipt.session)
        {
            if let Some(task) = resources.observer {
                task.abort();
            }
            if let Some(task) = resources.draft_save {
                task.abort();
            }
        }
    }

    pub(super) async fn flush_drafts(&mut self, state: &mut UiState) -> Result<(), String> {
        for resources in self.sessions.values_mut() {
            if let Some(task) = resources.draft_save.take() {
                task.abort();
            }
        }
        let exit_request = if !state.orphan_draft.is_empty() {
            Some(
                self.exit_draft_request
                    .get_or_insert_with(|| {
                        if let Some(pending) = &mut state.pending_create {
                            // Exiting preserves this text as a draft, never a bootstrap submission.
                            pending.first_input = None;
                            pending.source = crate::state::DraftSource::None;
                            CreateSessionRequest {
                                request_id: pending.request_id,
                                workspace: self.workspace,
                                title: pending.title.clone(),
                                provisional: pending.provisional,
                            }
                        } else {
                            CreateSessionRequest {
                                request_id: bone_app::RequestId::new(),
                                workspace: self.workspace,
                                title: "New conversation".into(),
                                provisional: true,
                            }
                        }
                    })
                    .clone(),
            )
        } else {
            None
        };

        let mut first_error = self.flush_title_writes().await.err();
        for (id, ui) in &state.session_ui {
            if ui.draft_revision > ui.saved_draft_revision {
                let session = match self.session(*id) {
                    Some(session) => Ok(session),
                    None => self.app.session(*id).await,
                };
                match session {
                    Ok(session) => {
                        if let Err(error) = session.save_draft(ui.draft.clone()).await {
                            first_error.get_or_insert_with(|| error.to_string());
                        }
                    }
                    Err(error) => {
                        first_error.get_or_insert_with(|| error.to_string());
                    }
                }
            }
        }
        if let Some(error) = first_error {
            return Err(error);
        }
        if let Some(request) = exit_request {
            let session = self
                .app
                .create_session_idempotent(request.clone())
                .await
                .map_err(|error| error.to_string())?;
            let snapshot = session
                .snapshot()
                .await
                .map_err(|error| error.to_string())?;
            session
                .save_draft(state.orphan_draft.clone())
                .await
                .map_err(|error| error.to_string())?;
            let info = snapshot.session;
            let ui = state
                .session_ui
                .entry(info.id)
                .or_insert_with(|| crate::state::SessionUi::new(info.clone(), 0));
            ui.draft = std::mem::take(&mut state.orphan_draft);
            ui.draft_cursor = state.orphan_cursor;
            ui.draft_revision = ui.draft_revision.wrapping_add(1);
            ui.saved_draft_revision = ui.draft_revision;
            ui.saved_draft = ui.draft.clone();
            // Prevent an in-flight hydration from replacing this just-saved buffer.
            ui.hydrated = true;
            state.orphan_cursor = 0;
            state.orphan_revision = state.orphan_revision.wrapping_add(1);
            if state
                .pending_create
                .as_ref()
                .is_some_and(|pending| pending.request_id == request.request_id)
            {
                state.pending_create = None;
            }
            if !state.sessions.iter().any(|known| known.id == info.id) {
                state.sessions.insert(0, info.clone());
            }
            self.retain_session(session);
            self.exit_draft_request = None;
        }
        Ok(())
    }

    async fn flush_title_writes(&self) -> Result<(), String> {
        let futures = self
            .session_operations
            .values()
            .map(|operation| operation.future.clone())
            .collect::<Vec<_>>();
        let mut first_error = None;
        for outcome in futures_util::future::join_all(futures).await {
            if let SessionOperationOutcome::Renamed(Err(error)) = outcome {
                first_error.get_or_insert_with(|| error.to_string());
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    pub(super) async fn shutdown(mut self) -> Result<(), bone_app::Error> {
        if let Some(login) = self.login.take() {
            login.abort();
        }
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

async fn model_applied(
    app: &App,
    workspace: bone_app::WorkspaceId,
    tx: &mpsc::Sender<UiEvent>,
    session: Option<SessionId>,
    request: u64,
    error: Option<bone_app::Error>,
) {
    let facts = super::models::facts(app, workspace, session).await;
    let label = facts
        .as_ref()
        .and_then(|facts| facts.saved.as_ref().ok())
        .map(|model| model.selection.model.clone());
    let _ = tx
        .send(UiEvent::ModelApplied {
            session,
            request,
            label,
            facts,
            error: error.map(|error| error.to_string()),
        })
        .await;
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;

    async fn runtime_with_provisional_session() -> (
        tempfile::TempDir,
        App,
        Session,
        Runtime,
        mpsc::Receiver<UiEvent>,
    ) {
        let root = tempfile::tempdir().unwrap();
        let app = App::open(bone_app::AppOptions::new(root.path().join("data")))
            .await
            .unwrap();
        let workspace = app.open_workspace(root.path()).await.unwrap();
        let session = app
            .create_session_idempotent(CreateSessionRequest {
                request_id: bone_app::RequestId::new(),
                workspace: workspace.id,
                title: "New conversation".into(),
                provisional: true,
            })
            .await
            .unwrap();
        let (tx, rx) = mpsc::channel(16);
        let (ready_tx, _ready_rx) = mpsc::channel(1);
        let runtime = Runtime::new(app.clone(), workspace.id, tx, ready_tx);
        (root, app, session, runtime, rx)
    }

    fn gated_operation(gate: Arc<tokio::sync::Notify>) -> SharedSessionOperation {
        async move {
            gate.notified().await;
            SessionOperationOutcome::AutoTitled(Ok(None))
        }
        .boxed()
        .shared()
    }

    #[tokio::test]
    async fn auto_title_then_manual_rename_are_durable_in_effect_order() {
        let (_root, app, session, mut runtime, _rx) = runtime_with_provisional_session().await;
        let id = session.snapshot().await.unwrap().session.id;

        runtime
            .apply(Effect::AutoTitle {
                session: id,
                generation: 1,
                request: 1,
                first_input: "Automatic title from this first input".into(),
            })
            .await;
        runtime
            .apply(Effect::RenameSession {
                session: id,
                request: 2,
                title: "Manual title".into(),
            })
            .await;

        runtime.flush_title_writes().await.unwrap();
        assert_eq!(
            app.session(id)
                .await
                .unwrap()
                .snapshot()
                .await
                .unwrap()
                .session
                .title,
            "Manual title"
        );
        runtime.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn title_after_release_waits_for_release_and_reopens_the_actor() {
        let (_root, app, session, mut runtime, _rx) = runtime_with_provisional_session().await;
        let id = session.snapshot().await.unwrap().session.id;
        let gate = Arc::new(tokio::sync::Notify::new());
        runtime.session_operations.insert(
            id,
            PendingSessionOperation {
                token: SessionOperationToken::AutoTitle(99),
                future: gated_operation(gate.clone()),
            },
        );

        runtime
            .apply(Effect::ReleaseSession {
                session: id,
                generation: 2,
            })
            .await;
        runtime
            .apply(Effect::RenameSession {
                session: id,
                request: 3,
                title: "Rename after release".into(),
            })
            .await;

        assert!(
            tokio::time::timeout(Duration::from_millis(10), runtime.flush_title_writes())
                .await
                .is_err(),
            "the outer exit deadline may stop waiting without cancelling the write chain"
        );
        assert_eq!(
            session.snapshot().await.unwrap().session.title,
            "New conversation"
        );
        gate.notify_one();
        runtime.flush_title_writes().await.unwrap();
        assert_eq!(
            app.session(id)
                .await
                .unwrap()
                .snapshot()
                .await
                .unwrap()
                .session
                .title,
            "Rename after release"
        );
        runtime.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn release_after_title_waits_until_the_title_is_durable() {
        let (_root, app, session, mut runtime, _rx) = runtime_with_provisional_session().await;
        let id = session.snapshot().await.unwrap().session.id;
        let gate = Arc::new(tokio::sync::Notify::new());
        runtime.session_operations.insert(
            id,
            PendingSessionOperation {
                token: SessionOperationToken::AutoTitle(99),
                future: gated_operation(gate.clone()),
            },
        );

        runtime
            .apply(Effect::RenameSession {
                session: id,
                request: 3,
                title: "Rename before release".into(),
            })
            .await;
        runtime
            .apply(Effect::ReleaseSession {
                session: id,
                generation: 4,
            })
            .await;
        tokio::task::yield_now().await;
        assert_eq!(
            session.snapshot().await.unwrap().session.title,
            "New conversation"
        );

        gate.notify_one();
        runtime.flush_title_writes().await.unwrap();
        assert_eq!(
            app.session(id)
                .await
                .unwrap()
                .snapshot()
                .await
                .unwrap()
                .session
                .title,
            "Rename before release"
        );
        runtime.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn exit_saves_orphan_to_existing_create_identity_without_submitting() {
        let root = tempfile::tempdir().unwrap();
        let app = App::open(bone_app::AppOptions::new(root.path().join("data")))
            .await
            .unwrap();
        let workspace = app.open_workspace(root.path()).await.unwrap();
        let request = CreateSessionRequest {
            request_id: bone_app::RequestId::new(),
            workspace: workspace.id,
            title: "New conversation".into(),
            provisional: true,
        };
        // Creation committed, but the UI has not yet received its acknowledgement.
        let original = app
            .create_session_idempotent(request.clone())
            .await
            .unwrap();
        let original_id = original.snapshot().await.unwrap().session.id;
        let (tx, _rx) = mpsc::channel(1);
        let (ready_tx, _ready_rx) = mpsc::channel(1);
        let mut runtime = Runtime::new(app.clone(), workspace.id, tx, ready_tx);
        let mut state = UiState::default();
        state.orphan_draft = "未发送的草稿\nsecond line".into();
        state.pending_create = Some(crate::state::PendingCreate {
            request_id: request.request_id,
            title: request.title,
            provisional: true,
            first_input: None,
            failed: true,
            source: crate::state::DraftSource::None,
        });
        runtime.flush_drafts(&mut state).await.unwrap();
        runtime.flush_drafts(&mut state).await.unwrap();
        let sessions = app.list_sessions(workspace.id).await.unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, original_id);
        let saved = original.snapshot().await.unwrap();
        assert_eq!(saved.draft, "未发送的草稿\nsecond line");
        assert!(saved.inputs.is_empty());
        assert!(
            original
                .history(bone_app::SessionSeq(0), 100)
                .await
                .unwrap()
                .items
                .iter()
                .all(|entry| !matches!(entry.event, bone_app::SessionEvent::InputSubmitted { .. }))
        );
        assert!(state.orphan_draft.is_empty());
        runtime.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn failed_exit_retains_orphan_and_request_identity() {
        let root = tempfile::tempdir().unwrap();
        let app = App::open(bone_app::AppOptions::new(root.path().join("data")))
            .await
            .unwrap();
        let (tx, _rx) = mpsc::channel(1);
        let (ready_tx, _ready_rx) = mpsc::channel(1);
        // An absent workspace makes App reject creation without changing the draft.
        let mut runtime = Runtime::new(app, bone_app::WorkspaceId::new(), tx, ready_tx);
        let mut state = UiState::default();
        state.orphan_draft = "keep me".into();
        assert!(runtime.flush_drafts(&mut state).await.is_err());
        let request = runtime.exit_draft_request.clone().unwrap();
        assert!(runtime.flush_drafts(&mut state).await.is_err());
        assert_eq!(runtime.exit_draft_request.as_ref(), Some(&request));
        assert_eq!(state.orphan_draft, "keep me");
        runtime.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn shutdown_aborts_login_observer() {
        let root = tempfile::tempdir().unwrap();
        let app = App::open(bone_app::AppOptions::new(root.path().join("data")))
            .await
            .unwrap();
        let (tx, _rx) = mpsc::channel(1);
        let (ready_tx, _ready_rx) = mpsc::channel(1);
        let mut runtime = Runtime::new(app, bone_app::WorkspaceId::new(), tx, ready_tx);
        let observer = tokio::spawn(std::future::pending::<()>());
        runtime.login = Some(observer.abort_handle());
        runtime.shutdown().await.unwrap();
        assert!(observer.await.unwrap_err().is_cancelled());
    }
}
