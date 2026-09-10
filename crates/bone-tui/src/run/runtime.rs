use std::{collections::BTreeMap, time::Duration};

use bone_app::{App, Session, SessionId};
use tokio::{sync::mpsc, task::AbortHandle};

use crate::state::{Effect, OperationKind, UiEvent, UiState};

use super::RunError;

const HISTORY_PAGE: usize = 32;
const DETAIL_PAGE: usize = 8;
const DRAFT_DEBOUNCE: Duration = Duration::from_millis(250);
const WORKSPACE_CHANGE_PAGE: usize = 64;
const WORKSPACE_FILE_BYTES: usize = 256 * 1024;
const EVIDENCE_PAGE: usize = 8;
const EVIDENCE_SOURCE_BYTES: usize = 64 * 1024;

pub(super) struct Runtime {
    app: App,
    workspace: bone_app::WorkspaceId,
    sessions: BTreeMap<SessionId, Session>,
    observers: BTreeMap<SessionId, AbortHandle>,
    draft_saves: BTreeMap<SessionId, AbortHandle>,
    logins: BTreeMap<bone_app::ProfileId, bone_app::LoginAttempt>,
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
            logins: BTreeMap::new(),
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
                        // Subscribe before hydration so a publish between snapshot and
                        // history cannot be missed forever by a newly-created watch.
                        let mut views = handle.observe();
                        let _snapshot = handle.snapshot().await?;
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
                        Err(error) => {
                            let _ = tx
                                .send(UiEvent::OperationFailed {
                                    kind: OperationKind::OpenSession,
                                    session: Some(session),
                                    generation: Some(generation),
                                    message: format!("会话无法打开：{error}"),
                                })
                                .await;
                        }
                    }
                });
            }
            Effect::CreateSession { title } => {
                let app = self.app.clone();
                let workspace = self.workspace;
                let ready_tx = self.ready_tx.clone();
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    match app.create_session(workspace, title).await {
                        Ok(session) => match session.snapshot().await {
                            Ok(snapshot) => {
                                let _ = ready_tx
                                    .send(SessionReady::Created {
                                        session,
                                        info: snapshot.session,
                                    })
                                    .await;
                            }
                            Err(error) => {
                                let _ = tx
                                    .send(UiEvent::OperationFailed {
                                        kind: OperationKind::CreateSession,
                                        session: None,
                                        generation: None,
                                        message: format!("新会话已创建，但暂时无法打开：{error}"),
                                    })
                                    .await;
                            }
                        },
                        Err(error) => {
                            let _ = tx
                                .send(UiEvent::OperationFailed {
                                    kind: OperationKind::CreateSession,
                                    session: None,
                                    generation: None,
                                    message: format!("无法创建会话：{error}"),
                                })
                                .await;
                        }
                    }
                });
            }
            Effect::RenameSession {
                session,
                generation,
                operation,
                title,
            } => {
                let handle = self.sessions.get(&session).cloned();
                let app = self.app.clone();
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let updated_title = title.clone();
                    let renamed = async {
                        let handle = match handle {
                            Some(handle) => handle,
                            None => app.session(session).await?,
                        };
                        handle.rename(title).await
                    }
                    .await;
                    let event = match renamed {
                        Ok(()) => UiEvent::SessionRenamed {
                            session,
                            generation,
                            operation,
                            title: updated_title,
                        },
                        Err(error) => UiEvent::SessionManagementFailed {
                            session,
                            generation,
                            operation,
                            message: format!("会话重命名失败：{error}"),
                        },
                    };
                    let _ = tx.send(event).await;
                });
            }
            Effect::ArchiveSession {
                session,
                generation,
                operation,
                archived,
            } => {
                let handle = self.sessions.get(&session).cloned();
                let app = self.app.clone();
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let changed = async {
                        let handle = match handle {
                            Some(handle) => handle,
                            None => app.session(session).await?,
                        };
                        handle.archive(archived).await
                    }
                    .await;
                    let event = match changed {
                        Ok(()) => UiEvent::SessionArchived {
                            session,
                            generation,
                            operation,
                            archived,
                        },
                        Err(error) => UiEvent::SessionManagementFailed {
                            session,
                            generation,
                            operation,
                            message: format!(
                                "会话{}失败：{error}",
                                if archived { "归档" } else { "恢复" }
                            ),
                        },
                    };
                    let _ = tx.send(event).await;
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
                    let saved_text = text.clone();
                    let saved = async {
                        let handle = match handle {
                            Some(handle) => handle,
                            None => app.session(session).await?,
                        };
                        handle.save_draft(text).await
                    }
                    .await;
                    let event = match saved {
                        Ok(()) => UiEvent::DraftSaved {
                            session,
                            generation,
                            revision,
                            text: saved_text,
                        },
                        Err(error) => UiEvent::OperationFailed {
                            kind: OperationKind::SaveDraft,
                            session: Some(session),
                            generation: Some(generation),
                            message: format!("草稿未保存：{error}"),
                        },
                    };
                    let _ = tx.send(event).await;
                });
                self.draft_saves.insert(session, task.abort_handle());
            }
            Effect::Submit {
                session,
                generation,
                input,
            } => {
                if let Some(handle) = self.sessions.get(&session).cloned() {
                    let tx = self.tx.clone();
                    let request_id = input.request_id;
                    tokio::spawn(async move {
                        let event = match handle.submit(input).await {
                            Ok(receipt) => UiEvent::Submitted {
                                session,
                                generation,
                                request_id,
                                receipt,
                            },
                            Err(error) => UiEvent::OperationFailed {
                                kind: OperationKind::Submit,
                                session: Some(session),
                                generation: Some(generation),
                                message: format!("要求未提交：{error}"),
                            },
                        };
                        let _ = tx.send(event).await;
                    });
                }
            }
            Effect::Stop {
                session,
                generation,
            } => {
                if let Some(handle) = self.sessions.get(&session).cloned() {
                    let tx = self.tx.clone();
                    tokio::spawn(async move {
                        if let Err(error) = handle.stop().await {
                            let _ = tx
                                .send(UiEvent::OperationFailed {
                                    kind: OperationKind::Stop,
                                    session: Some(session),
                                    generation: Some(generation),
                                    message: format!("停止请求失败：{error}"),
                                })
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
                        let event = match handle.history(after, HISTORY_PAGE).await {
                            Ok(page) => UiEvent::HistoryLoaded {
                                session,
                                generation,
                                page,
                            },
                            Err(error) => UiEvent::OperationFailed {
                                kind: OperationKind::LoadHistory,
                                session: Some(session),
                                generation: Some(generation),
                                message: format!("历史读取失败：{error}"),
                            },
                        };
                        let _ = tx.send(event).await;
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
                        let event = match handle.recent_history(Some(cursor), HISTORY_PAGE).await {
                            Ok(page) => UiEvent::OlderHistoryLoaded {
                                session,
                                generation,
                                page,
                            },
                            Err(error) => UiEvent::OperationFailed {
                                kind: OperationKind::LoadOlderHistory,
                                session: Some(session),
                                generation: Some(generation),
                                message: format!("更早历史读取失败：{error}"),
                            },
                        };
                        let _ = tx.send(event).await;
                    });
                }
            }
            Effect::RefreshOverview { generation } => {
                let app = self.app.clone();
                let workspace = self.workspace;
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let event = match app.workspace_overview(workspace).await {
                        Ok(overview) => UiEvent::OverviewLoaded {
                            generation,
                            sessions: overview
                                .sessions
                                .into_iter()
                                .map(|summary| summary.session)
                                .collect(),
                            attention: overview.attention,
                            attention_projection_pending: overview.attention_projection_pending,
                            unresolved_writes: overview.unresolved_writes,
                        },
                        Err(error) => UiEvent::OperationFailed {
                            kind: OperationKind::RefreshOverview,
                            session: None,
                            generation: None,
                            message: format!("工作区状态暂时无法刷新：{error}"),
                        },
                    };
                    let _ = tx.send(event).await;
                });
            }
            Effect::ReloadRecentHistory {
                session,
                generation,
            } => {
                if let Some(handle) = self.sessions.get(&session).cloned() {
                    let tx = self.tx.clone();
                    tokio::spawn(async move {
                        let event = match handle.recent_history(None, HISTORY_PAGE).await {
                            Ok(page) => UiEvent::RecentHistoryReloaded {
                                session,
                                generation,
                                page,
                            },
                            Err(error) => UiEvent::OperationFailed {
                                kind: OperationKind::ReloadRecentHistory,
                                session: Some(session),
                                generation: Some(generation),
                                message: format!("最近历史无法重新读取：{error}"),
                            },
                        };
                        let _ = tx.send(event).await;
                    });
                }
            }
            Effect::LoadSettings {
                session,
                generation,
                query,
            } => {
                let app = self.app.clone();
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let loaded = async {
                        let resolved = app.resolved_config(session).await?;
                        let profiles = app.profiles().await?;
                        Ok::<_, bone_app::Error>((resolved, profiles))
                    }
                    .await;
                    let event = match loaded {
                        Ok((resolved, profiles)) => {
                            UiEvent::SettingsLoaded(Box::new(crate::state::SettingsData {
                                session,
                                generation,
                                query,
                                resolved,
                                profiles,
                            }))
                        }
                        Err(error) => UiEvent::OperationFailed {
                            kind: OperationKind::LoadSettings,
                            session: Some(session),
                            generation: Some(generation),
                            message: format!("设置暂时无法读取：{error}"),
                        },
                    };
                    let _ = tx.send(event).await;
                });
            }
            Effect::LoadResults {
                session,
                generation,
                query,
            } => {
                let app = self.app.clone();
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let event = match app.results(session, None, DETAIL_PAGE).await {
                        Ok(page) => UiEvent::ResultsLoaded {
                            session,
                            generation,
                            query,
                            page,
                        },
                        Err(error) => UiEvent::OperationFailed {
                            kind: OperationKind::LoadResults,
                            session: Some(session),
                            generation: Some(generation),
                            message: format!("结果暂时无法读取：{error}"),
                        },
                    };
                    let _ = tx.send(event).await;
                });
            }
            Effect::LoadOlderResults {
                session,
                generation,
                query,
                cursor,
            } => {
                let app = self.app.clone();
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let event = match app.results(session, Some(cursor), DETAIL_PAGE).await {
                        Ok(page) => UiEvent::OlderResultsLoaded {
                            session,
                            generation,
                            query,
                            page,
                        },
                        Err(error) => UiEvent::OperationFailed {
                            kind: OperationKind::LoadOlderResults,
                            session: Some(session),
                            generation: Some(generation),
                            message: format!("更早结果暂时无法读取：{error}"),
                        },
                    };
                    let _ = tx.send(event).await;
                });
            }
            Effect::LoadAcceptances {
                session,
                generation,
                result,
                query,
                cursor,
                append_older,
            } => {
                let app = self.app.clone();
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let event = match app.acceptances(result, cursor, DETAIL_PAGE).await {
                        Ok(page) => UiEvent::AcceptancesLoaded {
                            session,
                            generation,
                            result,
                            query,
                            page,
                            append_older,
                        },
                        Err(error) => UiEvent::OperationFailed {
                            kind: OperationKind::LoadAcceptances,
                            session: Some(session),
                            generation: Some(generation),
                            message: format!("历史验收暂时无法读取：{error}"),
                        },
                    };
                    let _ = tx.send(event).await;
                });
            }
            Effect::SubmitAcceptance {
                session,
                generation,
                submission,
            } => {
                if let Some(handle) = self.sessions.get(&session).cloned() {
                    let tx = self.tx.clone();
                    let request_id = submission.request_id;
                    tokio::spawn(async move {
                        let event = match handle.submit_acceptance(submission).await {
                            Ok(receipt) => UiEvent::AcceptanceSubmitted {
                                session,
                                generation,
                                request_id,
                                receipt,
                            },
                            Err(error) => UiEvent::AcceptanceFailed {
                                session,
                                generation,
                                request_id,
                                message: format!("验收判定未保存：{error}"),
                            },
                        };
                        let _ = tx.send(event).await;
                    });
                }
            }
            Effect::UpdateConfig {
                session,
                generation,
                change,
            } => {
                let app = self.app.clone();
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let event = match app
                        .update_config(bone_app::ConfigScope::Session(session), change)
                        .await
                    {
                        Ok(_) => UiEvent::ConfigUpdated {
                            session,
                            generation,
                        },
                        Err(error) => UiEvent::ConfigUpdateFailed {
                            session,
                            generation,
                            message: format!(
                                "配置保存或应用没有完全成功；已重新读取实际状态：{error}"
                            ),
                        },
                    };
                    let _ = tx.send(event).await;
                });
            }
            Effect::StartLogin { profile, query } => {
                if let Some(previous) = self.logins.remove(&profile) {
                    previous.cancel();
                }
                match self.app.login(profile.clone()).await {
                    Ok(attempt) => {
                        let mut states = attempt.observe();
                        let tx = self.tx.clone();
                        let observed_profile = profile.clone();
                        tokio::spawn(async move {
                            loop {
                                let state = states.borrow_and_update().as_ref().clone();
                                if tx
                                    .send(UiEvent::LoginChanged {
                                        profile: observed_profile.clone(),
                                        query,
                                        state: state.clone(),
                                    })
                                    .await
                                    .is_err()
                                    || matches!(
                                        state,
                                        bone_app::LoginState::Succeeded
                                            | bone_app::LoginState::Failed { .. }
                                            | bone_app::LoginState::Cancelled
                                    )
                                {
                                    break;
                                }
                                if states.changed().await.is_err() {
                                    break;
                                }
                            }
                        });
                        self.logins.insert(profile, attempt);
                    }
                    Err(error) => {
                        let _ = self
                            .tx
                            .send(UiEvent::LoginChanged {
                                profile,
                                query,
                                state: bone_app::LoginState::Failed {
                                    message: error.to_string(),
                                },
                            })
                            .await;
                    }
                }
            }
            Effect::Logout { profile, query } => {
                if let Some(attempt) = self.logins.remove(&profile) {
                    attempt.cancel();
                }
                let app = self.app.clone();
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let state = match app.logout(profile.clone()).await {
                        Ok(()) => bone_app::LoginState::Cancelled,
                        Err(error) => bone_app::LoginState::Failed {
                            message: error.to_string(),
                        },
                    };
                    let _ = tx
                        .send(UiEvent::LoginChanged {
                            profile,
                            query,
                            state,
                        })
                        .await;
                });
            }
            Effect::ReloadOpenSessions { profile, query } => {
                let sessions = self.sessions.values().cloned().collect::<Vec<_>>();
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let mut failures = 0;
                    for session in sessions {
                        if session.reload_config().await.is_err() {
                            failures += 1;
                        }
                    }
                    let _ = tx
                        .send(UiEvent::ConfigsReloaded {
                            profile,
                            query,
                            failures,
                        })
                        .await;
                });
            }
            Effect::ReleaseSession {
                session,
                generation,
            } => {
                let app = self.app.clone();
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let event = match app.release_session(session).await {
                        Ok(receipt) => UiEvent::SessionReleased {
                            generation,
                            receipt,
                        },
                        Err(error) => UiEvent::OperationFailed {
                            kind: OperationKind::ReleaseSession,
                            session: Some(session),
                            generation: None,
                            message: format!("会话资源暂时无法释放：{error}"),
                        },
                    };
                    let _ = tx.send(event).await;
                });
            }
            Effect::ResolveWrite {
                session,
                call,
                resolution,
            } => {
                let handle = self.sessions.get(&session).cloned();
                let app = self.app.clone();
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let resolved = async {
                        let handle = match handle {
                            Some(handle) => handle,
                            None => app.session(session).await?,
                        };
                        handle.resolve_write(call, resolution).await
                    }
                    .await;
                    let event = match resolved {
                        Ok(_) => UiEvent::WriteResolved { session, call },
                        Err(error) => UiEvent::WriteResolutionFailed {
                            session,
                            call,
                            message: format!("核查结果未保存：{error}"),
                        },
                    };
                    let _ = tx.send(event).await;
                });
            }
            Effect::LoadWorkspaceChanges {
                workspace,
                query,
                cursor,
                append,
            } => {
                let app = self.app.clone();
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let event = match app
                        .workspace_changes(workspace, cursor, WORKSPACE_CHANGE_PAGE)
                        .await
                    {
                        Ok(page) => UiEvent::WorkspaceChangesLoaded {
                            workspace,
                            query,
                            page,
                            append,
                        },
                        Err(error) => UiEvent::WorkspaceChangesFailed {
                            workspace,
                            query,
                            message: format!("工作区变更暂时无法读取：{error}"),
                        },
                    };
                    let _ = tx.send(event).await;
                });
            }
            Effect::LoadWorkspaceFile {
                workspace,
                query,
                path,
                source,
                cursor,
                append,
            } => {
                let app = self.app.clone();
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let event = match app
                        .workspace_file_page(
                            workspace,
                            path.clone(),
                            source,
                            cursor.clone(),
                            WORKSPACE_FILE_BYTES,
                        )
                        .await
                    {
                        Ok(page) => UiEvent::WorkspaceFileLoaded {
                            workspace,
                            query,
                            cursor,
                            append,
                            page,
                        },
                        Err(error) => UiEvent::WorkspaceFileFailed {
                            workspace,
                            query,
                            path,
                            source,
                            cursor,
                            append,
                            message: format!("文件内容暂时无法读取：{error}"),
                        },
                    };
                    let _ = tx.send(event).await;
                });
            }
            Effect::LoadArtifact { result, query } => {
                let app = self.app.clone();
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let loaded = tokio::try_join!(
                        app.result_artifact(result),
                        app.result_evidence(result, None, EVIDENCE_PAGE),
                    );
                    let event = match loaded {
                        Ok((artifact, evidence)) => UiEvent::ArtifactLoaded {
                            result,
                            query,
                            artifact,
                            evidence,
                        },
                        Err(error) => UiEvent::ArtifactFailed {
                            result,
                            query,
                            message: format!("产物与证据暂时无法读取：{error}"),
                        },
                    };
                    let _ = tx.send(event).await;
                });
            }
            Effect::LoadEvidence {
                result,
                query,
                cursor,
            } => {
                let app = self.app.clone();
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let event = match app
                        .result_evidence(result, Some(cursor), EVIDENCE_PAGE)
                        .await
                    {
                        Ok(page) => UiEvent::EvidenceLoaded {
                            result,
                            query,
                            page,
                        },
                        Err(error) => UiEvent::ArtifactFailed {
                            result,
                            query,
                            message: format!("更多证据暂时无法读取：{error}"),
                        },
                    };
                    let _ = tx.send(event).await;
                });
            }
            Effect::LoadEvidenceSource {
                result,
                query,
                source,
                offset,
                append,
            } => {
                let app = self.app.clone();
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let event = match app
                        .evidence_source(result, source, offset, EVIDENCE_SOURCE_BYTES)
                        .await
                    {
                        Ok(page) => UiEvent::EvidenceSourceLoaded {
                            result,
                            query,
                            page,
                            append,
                        },
                        Err(error) => UiEvent::EvidenceSourceFailed {
                            result,
                            query,
                            message: format!("证据来源暂时无法读取：{error}"),
                        },
                    };
                    let _ = tx.send(event).await;
                });
            }
            Effect::SetApiKey {
                session,
                generation,
                profile,
                key,
            } => {
                let app = self.app.clone();
                let sessions = self.sessions.values().cloned().collect::<Vec<_>>();
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let result = match bone_app::ApiKey::new(key.into_string()) {
                        Ok(key) => app.set_api_key(profile.clone(), key).await,
                        Err(error) => Err(bone_app::Error::InvalidState(error.to_string())),
                    };
                    let event = match result {
                        Ok(()) => {
                            let mut reload_failures = 0;
                            for open in sessions {
                                if open.reload_config().await.is_err() {
                                    reload_failures += 1;
                                }
                            }
                            UiEvent::ApiKeySaved {
                                session,
                                generation,
                                profile,
                                reload_failures,
                            }
                        }
                        Err(error) => UiEvent::ApiKeyFailed {
                            session,
                            generation,
                            profile,
                            message: format!("API Key 未能保存：{error}"),
                        },
                    };
                    let _ = tx.send(event).await;
                });
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
        if self.observers.contains_key(&id) {
            return;
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
            SessionReady::Created { session, info } => {
                self.sessions.insert(info.id, session);
                Some(UiEvent::SessionCreated(info))
            }
        }
    }

    pub(super) fn accept_event(&mut self, event: &UiEvent, state: &UiState) {
        if let UiEvent::SessionReleased {
            generation,
            receipt,
        } = event
            && state
                .session_ui
                .get(&receipt.session)
                .is_some_and(|ui| ui.generation == *generation)
            && matches!(
                receipt.status,
                bone_app::SessionReleaseStatus::Released | bone_app::SessionReleaseStatus::NotOpen
            )
        {
            self.sessions.remove(&receipt.session);
            if let Some(observer) = self.observers.remove(&receipt.session) {
                observer.abort();
            }
            if let Some(save) = self.draft_saves.remove(&receipt.session) {
                save.abort();
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
                let session = if let Some(session) = self.sessions.get(id).cloned() {
                    Ok(session)
                } else {
                    self.app.session(*id).await
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

#[cfg(test)]
mod tests {
    use std::{fs, path::Path, process::Command};

    use super::*;
    use bone_app::{AppOptions, WorkspaceFileSource};

    fn git(workspace: &Path, arguments: &[&str]) {
        let output = Command::new("git")
            .args(arguments)
            .current_dir(workspace)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "BONE TUI test")
            .env("GIT_AUTHOR_EMAIL", "bone-tui@example.invalid")
            .env("GIT_COMMITTER_NAME", "BONE TUI test")
            .env("GIT_COMMITTER_EMAIL", "bone-tui@example.invalid")
            .output()
            .expect("run Git fixture command");
        assert!(
            output.status.success(),
            "git {arguments:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[tokio::test]
    async fn production_effect_runner_hydrates_a_real_app_session() {
        let temporary = tempfile::tempdir().unwrap();
        let app = App::open(AppOptions::new(temporary.path().join("data")))
            .await
            .unwrap();
        let workspace = app.open_workspace(temporary.path()).await.unwrap();
        let session = app.create_session(workspace.id, "真实会话").await.unwrap();
        session.save_draft("未发送草稿").await.unwrap();
        let info = session.snapshot().await.unwrap().session;

        let (tx, _rx) = mpsc::channel(16);
        let (ready_tx, mut ready_rx) = mpsc::channel(4);
        let mut runtime = Runtime::new(app.clone(), workspace.id, tx, ready_tx);
        runtime
            .apply(Effect::OpenSession {
                session: info.id,
                generation: 1,
            })
            .await
            .unwrap();

        let ready = ready_rx.recv().await.unwrap();
        let mut state = UiState::default();
        state
            .session_ui
            .insert(info.id, crate::state::SessionUi::new(info.clone(), 1));
        let event = runtime.accept_ready(ready, &state).unwrap();
        assert!(matches!(
            event,
            UiEvent::SessionOpened { session, .. } if session == info.id
        ));
        assert_eq!(
            runtime
                .sessions
                .get(&info.id)
                .unwrap()
                .snapshot()
                .await
                .unwrap()
                .draft,
            "未发送草稿"
        );

        drop(runtime);
        app.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn flush_drafts_persists_input_even_before_hydration() {
        let temporary = tempfile::tempdir().unwrap();
        let app = App::open(AppOptions::new(temporary.path().join("data")))
            .await
            .unwrap();
        let workspace = app.open_workspace(temporary.path()).await.unwrap();
        let session = app.create_session(workspace.id, "真实会话").await.unwrap();
        let info = session.snapshot().await.unwrap().session;
        drop(session);

        let (tx, _rx) = mpsc::channel(16);
        let (ready_tx, _ready_rx) = mpsc::channel(4);
        let mut runtime = Runtime::new(app.clone(), workspace.id, tx, ready_tx);
        let mut state = UiState {
            sessions: vec![info.clone()],
            selected: Some(info.id),
            ..UiState::default()
        };
        let ui = crate::state::SessionUi::new(info.clone(), 0);
        state.session_ui.insert(info.id, ui);
        let ui = state.session_ui.get_mut(&info.id).unwrap();
        ui.draft = "hydrate 前的重要输入".into();
        ui.draft_revision = 1;

        runtime.flush_drafts(&state).await.unwrap();
        assert_eq!(
            app.session(info.id)
                .await
                .unwrap()
                .snapshot()
                .await
                .unwrap()
                .draft,
            "hydrate 前的重要输入"
        );

        drop(runtime);
        app.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn production_effect_runner_reads_real_git_changes_and_bounded_file_views() {
        let temporary = tempfile::tempdir().unwrap();
        let data = temporary.path().join("data");
        let workspace_root = temporary.path().join("workspace");
        fs::create_dir(&workspace_root).unwrap();
        git(&workspace_root, &["init", "--quiet"]);
        fs::write(workspace_root.join("tracked.txt"), "before\n").unwrap();
        git(&workspace_root, &["add", "tracked.txt"]);
        git(&workspace_root, &["commit", "--quiet", "-m", "fixture"]);
        fs::write(workspace_root.join("tracked.txt"), "before\nafter 变更\n").unwrap();
        fs::write(workspace_root.join("新文件.txt"), "untracked 正文\n").unwrap();
        fs::write(
            workspace_root.join("large.txt"),
            vec![b'x'; WORKSPACE_FILE_BYTES + 1024],
        )
        .unwrap();

        let app = App::open(AppOptions::new(data)).await.unwrap();
        let workspace = app.open_workspace(&workspace_root).await.unwrap();
        let (tx, mut rx) = mpsc::channel(16);
        let (ready_tx, _ready_rx) = mpsc::channel(1);
        let mut runtime = Runtime::new(app.clone(), workspace.id, tx, ready_tx);

        runtime
            .apply(Effect::LoadWorkspaceChanges {
                workspace: workspace.id,
                query: 41,
                cursor: None,
                append: false,
            })
            .await
            .unwrap();
        let event = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("workspace changes effect deadline")
            .expect("workspace changes event");
        let UiEvent::WorkspaceChangesLoaded {
            workspace: event_workspace,
            query,
            page,
            append,
        } = event
        else {
            panic!("expected successful workspace changes event, got {event:?}");
        };
        assert_eq!(event_workspace, workspace.id);
        assert_eq!(query, 41);
        assert!(!append);
        assert!(matches!(
            page.baseline,
            bone_app::WorkspaceBaseline::Git { head: Some(_) }
        ));
        assert!(page.files.iter().any(|file| file.path == "tracked.txt"));
        assert!(page.files.iter().any(|file| file.path == "新文件.txt"));

        runtime
            .apply(Effect::LoadWorkspaceFile {
                workspace: workspace.id,
                query: 42,
                path: "tracked.txt".into(),
                source: WorkspaceFileSource::DiffAgainstHead,
                cursor: None,
                append: false,
            })
            .await
            .unwrap();
        let event = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("workspace diff effect deadline")
            .expect("workspace diff event");
        let UiEvent::WorkspaceFileLoaded {
            workspace: event_workspace,
            query,
            cursor,
            append,
            page: file,
        } = event
        else {
            panic!("expected successful workspace file event, got {event:?}");
        };
        assert_eq!(event_workspace, workspace.id);
        assert_eq!(query, 42);
        assert!(cursor.is_none());
        assert!(!append);
        assert_eq!(file.path, "tracked.txt");
        assert_eq!(file.source, WorkspaceFileSource::DiffAgainstHead);
        assert_eq!(file.media, bone_app::WorkspaceFileMedia::Text);
        assert!(file.text.as_deref().unwrap().contains("+after 变更"));
        assert!(file.next_cursor.is_none());

        runtime
            .apply(Effect::LoadWorkspaceFile {
                workspace: workspace.id,
                query: 43,
                path: "新文件.txt".into(),
                source: WorkspaceFileSource::WorkingTree,
                cursor: None,
                append: false,
            })
            .await
            .unwrap();
        let event = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("workspace body effect deadline")
            .expect("workspace body event");
        let UiEvent::WorkspaceFileLoaded {
            query, page: file, ..
        } = event
        else {
            panic!("expected successful untracked file event, got {event:?}");
        };
        assert_eq!(query, 43);
        assert_eq!(file.source, WorkspaceFileSource::WorkingTree);
        assert_eq!(file.text.as_deref(), Some("untracked 正文\n"));

        runtime
            .apply(Effect::LoadWorkspaceFile {
                workspace: workspace.id,
                query: 44,
                path: "large.txt".into(),
                source: WorkspaceFileSource::WorkingTree,
                cursor: None,
                append: false,
            })
            .await
            .unwrap();
        let event = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("bounded workspace body effect deadline")
            .expect("bounded workspace body event");
        let UiEvent::WorkspaceFileLoaded {
            query, page: file, ..
        } = event
        else {
            panic!("expected successful bounded file event, got {event:?}");
        };
        assert_eq!(query, 44);
        assert_eq!(file.offset, 0);
        assert_eq!(file.bytes_read, WORKSPACE_FILE_BYTES as u64);
        assert_eq!(
            file.text.as_deref().map(str::len),
            Some(WORKSPACE_FILE_BYTES)
        );
        let cursor = file
            .next_cursor
            .expect("large file must expose a second page");

        runtime
            .apply(Effect::LoadWorkspaceFile {
                workspace: workspace.id,
                query: 45,
                path: "large.txt".into(),
                source: WorkspaceFileSource::WorkingTree,
                cursor: Some(cursor.clone()),
                append: true,
            })
            .await
            .unwrap();
        let event = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("second workspace body page deadline")
            .expect("second workspace body page event");
        let UiEvent::WorkspaceFileLoaded {
            query,
            cursor: event_cursor,
            append,
            page: file,
            ..
        } = event
        else {
            panic!("expected successful second file page event, got {event:?}");
        };
        assert_eq!(query, 45);
        assert_eq!(event_cursor, Some(cursor));
        assert!(append);
        assert_eq!(file.offset, WORKSPACE_FILE_BYTES as u64);
        assert_eq!(file.bytes_read, 1024);
        assert_eq!(file.text.as_deref().map(str::len), Some(1024));
        assert!(file.next_cursor.is_none());

        drop(runtime);
        tokio::time::timeout(Duration::from_secs(5), app.shutdown())
            .await
            .expect("App shutdown deadline")
            .unwrap();
    }
}
