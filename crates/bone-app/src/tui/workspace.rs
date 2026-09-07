//! TUI application shell for durable directory-scoped workspaces.
//!
//! This product entry point opens Workspace/Session state before configuration
//! or network connection, persists input before an Agent sees it, and attaches
//! runtimes only when a user actually sends work.

use std::collections::{HashMap, HashSet};

use crate::{
    JournalRead, ModelSelection, Scope, SessionDraft, SessionLifecycle, SessionStoreError,
    SettingsService, WorkspaceApplication,
};
use bone_agent::{AgentHost, ShutdownReport, TaskConfig};
use crossterm::event::EventStream;
use futures_util::{StreamExt, future::join_all, stream::FuturesUnordered};
use tokio::sync::mpsc;

use super::{
    TuiError,
    app::{Action, App, AppEvent, UiSessionId},
    command_effects::{CommandContext, handle_command},
    report_notice,
    runtime_driver::{
        ConnectionTask, LiveSession, SessionUpdate, StartTask, apply_enqueued_pending_starts,
        busy_session_ids, enqueue_connection, enqueue_pending_starts, observe_session,
        persist_pending_retryable_statuses, task_model,
    },
    session_controller::{
        DurableUiSession, PendingRuntimeTurn, accept_post, activate_writer_session, create_session,
        model_is_ready, persist_draft, persist_interruption, persist_runtime_attached,
        persist_runtime_receipt, persist_runtime_records, persist_runtime_retryable,
        persist_runtime_stopping, persist_unresolved_shutdown_effects, prepare_post,
        release_idle_writer_leases_after_switch, turn_state,
    },
    terminal::TerminalSession,
    view,
};

pub async fn run_workspace(
    application: WorkspaceApplication,
    settings: Result<SettingsService, String>,
    initial_model: Option<String>,
) -> Result<Vec<ShutdownReport>, TuiError> {
    // This is the product startup boundary for the selected conversation. The
    // lease stays in `durable` for the full runtime lifetime; it is not a
    // short record lock that can be dropped before recovery or posting work.
    let opened = application.open_or_create_writer_draft()?;
    let mut opened_lease = Some(opened.lease);
    let opened = opened.draft;
    let mut settings_problem = None;
    let settings = match settings {
        Ok(settings) => Some(settings),
        Err(error) => {
            // Keep the shell alive. The selected session will tell the user
            // why it cannot attach until the Settings Center repair flow lands.
            settings_problem = Some(format!(
                "Settings need repair before BONE can start: {error}"
            ));
            None
        }
    };

    if let (Some(model), Some(service)) = (initial_model, settings.as_ref()) {
        match ModelSelection::new(model, None, None) {
            Ok(selection) => match service.set_solver_model(
                application.workspace().id(),
                opened.record.id,
                Scope::Session(opened.record.id),
                selection,
            ) {
                Ok(_) => {}
                Err(error) => settings_problem = Some(format!("Could not apply --model: {error}")),
            },
            Err(error) => settings_problem = Some(format!("Invalid --model: {error}")),
        }
    }

    let show_progress = settings
        .as_ref()
        .and_then(|service| {
            service
                .display_settings()
                .ok()
                .map(|(display, _)| display.show_progress)
        })
        .unwrap_or(true);
    let listing = application.sessions().list()?;
    let mut app = App::new(application.workspace().display_root().display().to_string());
    let mut durable = HashMap::new();
    let mut next_ui_id = 1_u64;

    for record in listing
        .records
        .into_iter()
        .filter(|record| record.status.lifecycle == SessionLifecycle::Active)
    {
        let ui_id = UiSessionId(next_ui_id);
        next_ui_id = next_ui_id.saturating_add(1);
        let (journal, mut journal_read, journal_problem) =
            match application.sessions().journal(record.id) {
                Ok(journal) => match journal.read() {
                    Ok(read) => (Some(journal), read, None),
                    Err(error) => (
                        Some(journal),
                        JournalRead::default(),
                        Some(format!("history is unavailable: {error}")),
                    ),
                },
                Err(error) => (
                    None,
                    JournalRead::default(),
                    Some(format!("history is unavailable: {error}")),
                ),
            };
        let (next_turn, active_turn) = turn_state(&journal_read);
        let select = record.id == opened.record.id;
        let writer_lease = if select { opened_lease.take() } else { None };
        durable.insert(
            ui_id,
            DurableUiSession {
                record,
                journal,
                writer_lease,
                next_turn,
                active_turn,
                runtime_record_cursor: 0,
            },
        );
        // Never touch background sessions at startup. The selected session
        // already owns its lease from `open_or_create_writer_draft`; all
        // other sessions remain visible but read-only until deliberately
        // selected and lazily activated.
        if select {
            let _ = activate_writer_session(
                &application,
                settings.as_ref(),
                &mut durable,
                &mut app,
                ui_id,
                true,
            );
            if let Some(journal) = durable
                .get(&ui_id)
                .and_then(|session| session.journal.as_ref())
            {
                match journal.read() {
                    Ok(read) => journal_read = read,
                    Err(error) => {
                        report_notice(
                            &mut app,
                            format!("Could not reload this conversation after recovery: {error}"),
                        );
                    }
                }
            }
        }
        let ready_to_attach = durable
            .get(&ui_id)
            .is_some_and(|session| model_is_ready(settings.as_ref(), &session.record));
        let writer_available = durable
            .get(&ui_id)
            .is_some_and(|session| session.writer_lease.is_some());
        let record = &durable
            .get(&ui_id)
            .expect("a hydrated durable session was just inserted")
            .record;
        let _ = app.reduce(AppEvent::SessionHydrated {
            id: ui_id,
            record,
            journal: &journal_read,
            show_progress,
            ready_to_attach,
            writer_available,
            select,
        });
        // A broken background journal must not replace its local read-only
        // overlay with a writable setup state. The owner sees the full repair
        // message through `activate_writer_session`.
        if select && let Some(problem) = journal_problem {
            let _ = app.reduce(AppEvent::SessionNeedsSetup {
                id: ui_id,
                message: problem,
            });
        }
    }

    // `open_or_create_writer_draft` guarantees a healthy active record. Retaining a
    // defensive fallback here keeps a future filtered/repair listing from
    // panicking the renderer.
    if !app.has_sessions() {
        return Err(TuiError::SessionStore(SessionStoreError::NotFound(
            opened.record.id,
        )));
    }
    if let Some(problem) = settings_problem {
        let _ = app.reduce(AppEvent::Notice { message: problem });
    }
    if !opened.issues.is_empty() || !listing.issues.is_empty() {
        let _ = app.reduce(AppEvent::Notice {
            message: "Some saved conversations need recovery; healthy sessions remain available"
                .into(),
        });
    }

    let (updates, update_rx) = mpsc::channel(256);
    let (login_tx, mut login_rx) = mpsc::unbounded_channel();
    let mut live_sessions = Vec::<LiveSession>::new();
    // The TaskConfig used to construct each live runtime. A current runtime
    // cannot hot-swap its ModelAdapter, so this prevents us from recording a
    // new model in the journal while silently sending work to the old one.
    let mut live_tasks = HashMap::<UiSessionId, TaskConfig>::new();
    let mut pending_tasks = HashMap::<UiSessionId, PendingRuntimeTurn>::new();
    let mut host = None::<AgentHost>;
    let mut connecting = FuturesUnordered::<ConnectionTask>::new();
    let mut starting = FuturesUnordered::<StartTask>::new();
    let mut starting_ids = HashSet::<UiSessionId>::new();
    let workspace = application.workspace().canonical_root().to_path_buf();

    let ui_result: Result<(), TuiError> = async {
        let mut update_rx = update_rx;
        let mut terminal = TerminalSession::enter()?;
        let mut input = EventStream::new();

        loop {
            let mut viewport = Default::default();
            terminal.draw(|frame| viewport = view::render(frame, &app))?;
            let _ = app.reduce(AppEvent::ViewportMeasured { viewport });

            tokio::select! {
                event = input.next() => {
                    let Some(event) = event else { return Ok(()); };
                    let event = event?;
                    // A selected background session obtains its lease before
                    // the reducer sees a potentially mutating key. If another
                    // process owns it, the reducer receives the read-only
                    // overlay and cannot create a ghost composer change.
                    let selected_before = app.current_id();
                    let _ = activate_writer_session(
                        &application,
                        settings.as_ref(),
                        &mut durable,
                        &mut app,
                        selected_before,
                        false,
                    );
                    let action = app.reduce(AppEvent::Terminal(event));
                    let selected_after = app.current_id();
                    if selected_after != selected_before {
                        let acquired = activate_writer_session(
                            &application,
                            settings.as_ref(),
                            &mut durable,
                            &mut app,
                            selected_after,
                            false,
                        );
                        if acquired {
                            let busy_ids =
                                busy_session_ids(&live_sessions, &pending_tasks, &starting_ids);
                            release_idle_writer_leases_after_switch(
                                &mut durable,
                                &mut app,
                                &busy_ids,
                            );
                        }
                    }
                    match action {
                        Action::None => {}
                        Action::DraftChanged { id, draft } => {
                            let result = persist_draft(
                                &application,
                                &mut durable,
                                id,
                                draft.clone(),
                            );
                            let event = match result {
                                Ok(()) => AppEvent::DraftPersisted { id, draft },
                                Err(reason) => AppEvent::DraftPersistenceFailed {
                                    id,
                                    draft,
                                    reason,
                                },
                            };
                            let _ = app.reduce(event);
                        }
                        Action::Post { id, text } => {
                            if durable.get(&id).and_then(|session| session.active_turn).is_some() {
                                report_notice(
                                    &mut app,
                                    "This conversation is still processing a saved turn. Your draft is kept; wait for it to finish or stop it before sending another message.",
                                );
                                continue;
                            }
                            let prepared = match prepare_post(settings.as_ref(), &durable, id) {
                                Ok(prepared) => prepared,
                                Err(message) => {
                                    let _ = app.reduce(AppEvent::TurnRejected { id, reason: message });
                                    continue;
                                }
                            };
                            let live_index = live_sessions.iter().position(|session| session.id == id);
                            if let Some(current_task) = live_tasks.get(&id)
                                && current_task != &prepared.task
                            {
                                let _ = app.reduce(AppEvent::TurnRejected { id, reason: format!(
                                    "The attached runtime is pinned to {}. The saved {} selection will be used by a new conversation or a future runtime-restart flow; this draft is unchanged.",
                                    task_model(current_task),
                                    task_model(&prepared.task),
                                ) });
                                continue;
                            }
                            let queue_runtime = live_index.is_none();
                            match accept_post(
                                &application,
                                &mut durable,
                                id,
                                text,
                                prepared,
                                queue_runtime,
                            ) {
                                Ok(accepted) => {
                                    let _ = app.reduce(AppEvent::TurnAccepted {
                                        id,
                                        entry: &accepted.entry,
                                        text: accepted.text.clone(),
                                        queue_runtime,
                                    });
                                    let empty_draft = SessionDraft::empty();
                                    // The journal is already the durable
                                    // acceptance authority. A secondary
                                    // SessionRecord CAS failure must not be
                                    // misreported as an unsaved visible draft.
                                    let _ = app.reduce(AppEvent::DraftPersisted {
                                        id,
                                        draft: empty_draft,
                                    });
                                    if let Some(error) = accepted.record_persistence_error.as_ref() {
                                        report_notice(
                                            &mut app,
                                            format!(
                                                "Message is safely saved, but its session summary could not be updated: {error}"
                                            ),
                                        );
                                    }

                                    if let Some(index) = live_index {
                                        match live_sessions[index].agent.post(accepted.text.clone()).await {
                                            Ok(_) => {
                                                let _ = app.reduce(AppEvent::PendingPostAcknowledged { id });
                                                if let Err(error) = persist_runtime_receipt(
                                                    &application,
                                                    &mut durable,
                                                    id,
                                                    accepted.turn,
                                                    &accepted.effective_config_revision,
                                                    &accepted.solver_model,
                                                )
                                                {
                                                    report_notice(
                                                        &mut app,
                                                        format!(
                                                            "The runtime accepted your saved message, but its durable status could not be updated: {error}"
                                                        ),
                                                    );
                                                }
                                            }
                                            Err(error) => {
                                                // The turn itself is durable, but this particular
                                                // runtime never acknowledged it. Keep it pending
                                                // for an explicit /login retry instead of silently
                                                // assigning it to a different turn.
                                                if let Err(status_error) =
                                                    persist_runtime_retryable(&application, &mut durable, id)
                                                {
                                                    report_notice(
                                                        &mut app,
                                                        format!(
                                                            "Saved message was not delivered, and its durable retry state could not be updated: {status_error}"
                                                        ),
                                                    );
                                                }
                                                live_tasks.remove(&id);
                                                pending_tasks
                                                    .insert(id, accepted.pending_runtime_turn());
                                                let _ = app.reduce(AppEvent::RuntimeStartQueued {
                                                    id,
                                                    text: accepted.text,
                                                });
                                                let _ = app.reduce(AppEvent::RuntimeStartFailed {
                                                    id,
                                                    reason: format!(
                                                        "Saved message was not delivered: {error}. Use /login to retry."
                                                    ),
                                                });
                                                drop(live_sessions.swap_remove(index));
                                            }
                                        }
                                    } else {
                                        pending_tasks
                                            .insert(id, accepted.pending_runtime_turn());

                                        if let Some(host) = host.as_ref() {
                                            let started = enqueue_pending_starts(
                                                &application,
                                                &mut durable,
                                                &mut starting,
                                                &mut starting_ids,
                                                host,
                                                &workspace,
                                                &pending_tasks,
                                            );
                                            apply_enqueued_pending_starts(&mut app, started);
                                        } else if connecting.is_empty() {
                                            if let Some(service) = settings.as_ref() {
                                                let _ = app.reduce(AppEvent::ConnectionStarting);
                                                enqueue_connection(&mut connecting, service, login_tx.clone());
                                            } else {
                                                let _ = app.reduce(AppEvent::SessionNeedsSetup {
                                                    id,
                                                    message: "Settings need repair before BONE can connect".into(),
                                                });
                                            }
                                        }
                                    }
                                }
                                Err(message) => {
                                    let _ = app.reduce(AppEvent::TurnRejected { id, reason: message });
                                }
                            }
                        }
                        Action::Stop { id, clear } => {
                            if let Some(session) = live_sessions.iter().find(|session| session.id == id) {
                                let has_active_turn = durable
                                    .get(&id)
                                    .and_then(|durable| durable.active_turn)
                                    .is_some();
                                match session.agent.stop().await {
                                    Ok(()) => {
                                        if has_active_turn
                                            && let Err(error) =
                                            persist_runtime_stopping(&application, &mut durable, id)
                                        {
                                            report_notice(
                                                &mut app,
                                                format!(
                                                    "Stop was requested, but its durable status could not be updated: {error}"
                                                ),
                                            );
                                        }
                                        if clear {
                                            let _ = app.reduce(AppEvent::ComposerCleared { id });
                                        }
                                    }
                                    Err(error) => {
                                        let _ = app.reduce(AppEvent::RuntimeClosed {
                                            id,
                                            reason: format!("stop failed: {error}"),
                                        });
                                    }
                                }
                            }
                        }
                        Action::Command { id, command } => {
                            let _ = app.reduce(AppEvent::ComposerCleared { id });
                            let mut command_context = CommandContext {
                                application: &application,
                                settings: &settings,
                                durable: &mut durable,
                                show_progress,
                                next_ui_id: &mut next_ui_id,
                                app: &mut app,
                            };
                            let connect = handle_command(&mut command_context, id, command);
                            // `/resume` changes selection inside the command
                            // executor rather than the terminal reducer. Make
                            // its target writable (or visibly read-only)
                            // before the next user edit can reach it.
                            let selected_after_command = app.current_id();
                            let acquired = activate_writer_session(
                                &application,
                                settings.as_ref(),
                                &mut durable,
                                &mut app,
                                selected_after_command,
                                false,
                            );
                            if selected_after_command != id && acquired {
                                let busy_ids =
                                    busy_session_ids(&live_sessions, &pending_tasks, &starting_ids);
                                release_idle_writer_leases_after_switch(
                                    &mut durable,
                                    &mut app,
                                    &busy_ids,
                                );
                            }
                            if connect {
                                if let Some(host) = host.as_ref() {
                                    let started = enqueue_pending_starts(
                                        &application,
                                        &mut durable,
                                        &mut starting,
                                        &mut starting_ids,
                                        host,
                                        &workspace,
                                        &pending_tasks,
                                    );
                                    apply_enqueued_pending_starts(&mut app, started);
                                } else if connecting.is_empty()
                                    && let Some(service) = settings.as_ref()
                                {
                                    let _ = app.reduce(AppEvent::ConnectionStarting);
                                    enqueue_connection(&mut connecting, service, login_tx.clone());
                                }
                            }
                        }
                        Action::NewSession => {
                            let selected_before_new = app.current_id();
                            create_session(&application, &mut durable, &mut app, show_progress, &mut next_ui_id, settings.as_ref());
                            let selected_after_new = app.current_id();
                            let acquired = durable
                                .get(&selected_after_new)
                                .is_some_and(|session| session.writer_lease.is_some());
                            if selected_after_new != selected_before_new && acquired {
                                let busy_ids =
                                    busy_session_ids(&live_sessions, &pending_tasks, &starting_ids);
                                release_idle_writer_leases_after_switch(
                                    &mut durable,
                                    &mut app,
                                    &busy_ids,
                                );
                            }
                        }
                        Action::Quit => return Ok(()),
                    }
                }
                joined = connecting.next(), if !connecting.is_empty() => {
                    let joined = joined.expect("a pending connection exists");
                    match joined {
                        Ok(Ok(connected)) => {
                            let _ = app.reduce(AppEvent::ConnectionSucceeded);
                            report_notice(&mut app, "Connected. Saved messages are starting now.");
                            host = Some(connected.clone());
                            let started = enqueue_pending_starts(
                                &application,
                                &mut durable,
                                &mut starting,
                                &mut starting_ids,
                                &connected,
                                &workspace,
                                &pending_tasks,
                            );
                            apply_enqueued_pending_starts(&mut app, started);
                        }
                        Ok(Err(error)) => {
                            let _ = app.reduce(AppEvent::ConnectionFailed {
                                reason: format!(
                                    "{error}. Saved messages are waiting for a connection; use /login to retry."
                                ),
                                affected: pending_tasks.keys().copied().collect(),
                            });
                            persist_pending_retryable_statuses(
                                &application,
                                &mut durable,
                                &mut app,
                                &pending_tasks,
                            );
                        }
                        Err(error) => {
                            let _ = app.reduce(AppEvent::ConnectionFailed {
                                reason: format!(
                                    "Connection task stopped: {error}. Saved messages are waiting for a connection; use /login to retry."
                                ),
                                affected: pending_tasks.keys().copied().collect(),
                            });
                            persist_pending_retryable_statuses(
                                &application,
                                &mut durable,
                                &mut app,
                                &pending_tasks,
                            );
                        }
                    }
                }
                opened = starting.next(), if !starting.is_empty() => {
                    let joined = opened.expect("a pending runtime start exists");
                    match joined {
                        Ok((id, Ok((agent, observation)))) => {
                            starting_ids.remove(&id);
                            let Some(pending_turn) = pending_tasks.get(&id).cloned() else {
                                let _ = agent.shutdown().await;
                                continue;
                            };
                            if let Some(session) = durable.get_mut(&id) {
                                // A newly created Agent runtime starts its record cursor at
                                // one, independent of prior runtime attachments.
                                session.runtime_record_cursor = 0;
                            }
                            let _ = app.reduce(AppEvent::RuntimeAttached {
                                id,
                                snapshot: &observation.snapshot,
                            });
                            let pending_text = app.pending_post(id).map(str::to_owned);
                            if let Err(error) = persist_runtime_attached(
                                &application,
                                &mut durable,
                                id,
                                pending_text.is_some(),
                            ) {
                                report_notice(
                                    &mut app,
                                    format!(
                                        "Runtime is attached, but its durable status could not be updated: {error}"
                                    ),
                                );
                            }
                            if let Err(message) = persist_runtime_records(
                                &application,
                                &mut durable,
                                id,
                                &observation.snapshot.record,
                            ) {
                                report_notice(&mut app, message);
                            }
                            let observer = tokio::spawn(observe_session(
                                id,
                                agent.clone(),
                                observation,
                                updates.clone(),
                            ));
                            if let Some(text) = pending_text {
                                match agent.post(text).await {
                                    Ok(_) => {
                                        let _ = app.reduce(AppEvent::PendingPostAcknowledged { id });
                                        if let Err(error) = persist_runtime_receipt(
                                            &application,
                                            &mut durable,
                                            id,
                                            pending_turn.turn,
                                            &pending_turn.effective_config_revision,
                                            &pending_turn.solver_model,
                                        ) {
                                            report_notice(
                                                &mut app,
                                                format!(
                                                    "The runtime accepted your saved message, but its durable status could not be updated: {error}"
                                                ),
                                            );
                                        }
                                        pending_tasks.remove(&id);
                                        live_tasks.insert(id, pending_turn.task);
                                        live_sessions.push(LiveSession { id, agent, observer });
                                    }
                                    Err(error) => {
                                        observer.abort();
                                        let _ = agent.shutdown().await;
                                        if let Err(status_error) =
                                            persist_runtime_retryable(&application, &mut durable, id)
                                        {
                                            report_notice(
                                                &mut app,
                                                format!(
                                                    "Saved message was not delivered, and its durable retry state could not be updated: {status_error}"
                                                ),
                                            );
                                        }
                                        let _ = app.reduce(AppEvent::RuntimeStartFailed {
                                            id,
                                            reason: format!(
                                                "Saved message was not delivered: {error}. Use /login to retry."
                                            ),
                                        });
                                    }
                                }
                            } else {
                                // No new product path starts a runtime without a pending
                                // durable turn, but retain this defensive branch for a
                                // future explicit attach operation.
                                pending_tasks.remove(&id);
                                live_tasks.insert(id, pending_turn.task);
                                live_sessions.push(LiveSession { id, agent, observer });
                            }
                        }
                        Ok((id, Err(error))) => {
                            starting_ids.remove(&id);
                            let _ = app.reduce(AppEvent::RuntimeStartFailed {
                                id,
                                reason: format!(
                                    "Saved message could not start: {error}. Use /login to retry."
                                ),
                            });
                            if let Err(status_error) =
                                persist_runtime_retryable(&application, &mut durable, id)
                            {
                                report_notice(
                                    &mut app,
                                    format!(
                                        "Saved message could not start, and its durable retry state could not be updated: {status_error}"
                                    ),
                                );
                            }
                            report_notice(&mut app, format!("Could not start saved message: {error}"));
                        }
                        Err(error) => {
                            // A cancelled task does not expose its UiSessionId. Clear this
                            // small in-memory dedupe set; retry remains explicit through
                            // /login and never changes durable turn facts.
                            starting_ids.clear();
                            let reason = format!(
                                "Runtime startup task stopped: {error}. Use /login to retry."
                            );
                            for id in pending_tasks.keys().copied().collect::<Vec<_>>() {
                                let _ = app.reduce(AppEvent::RuntimeStartFailed {
                                    id,
                                    reason: reason.clone(),
                                });
                            }
                            persist_pending_retryable_statuses(
                                &application,
                                &mut durable,
                                &mut app,
                                &pending_tasks,
                            );
                            report_notice(&mut app, reason);
                        }
                    }
                }
                update = update_rx.recv() => match update {
                    Some(SessionUpdate::Step { id, step }) => {
                        if let Err(message) =
                            persist_runtime_records(&application, &mut durable, id, &step.records)
                        {
                            report_notice(&mut app, message);
                        }
                        let _ = app.reduce(AppEvent::RuntimeStep { id, step: &step });
                    }
                    Some(SessionUpdate::Reset { id, snapshot }) => {
                        if let Err(message) =
                            persist_runtime_records(
                                &application,
                                &mut durable,
                                id,
                                &snapshot.record,
                            )
                        {
                            report_notice(&mut app, message);
                        }
                        let _ = app.reduce(AppEvent::RuntimeReset { id, snapshot: &snapshot });
                    }
                    Some(SessionUpdate::Closed { id }) => {
                        if let Err(message) =
                            persist_interruption(
                                &application,
                                &mut durable,
                                id,
                                "agent runtime closed",
                            )
                        {
                            report_notice(&mut app, message);
                        }
                        let _ = app.reduce(AppEvent::RuntimeClosed {
                            id,
                            reason: "agent runtime closed".into(),
                        });
                        live_tasks.remove(&id);
                        live_sessions.retain(|session| session.id != id);
                    }
                    None => return Ok(()),
                },
                prompt = login_rx.recv() => if let Some(prompt) = prompt {
                    report_notice(&mut app, format!(
                        "Login required: open {} · code {} (do not share it)",
                        prompt.verification_uri, prompt.user_code
                    ));
                }
            }
        }
    }
    .await;

    // The runtime cannot survive this process. Snapshot each actor before
    // recording an interruption: an observer may already have queued a final
    // Step while terminal input won the `select!` race. The snapshot is the
    // authoritative last-chance projection and prevents a completed turn from
    // being falsely written as interrupted on exit.
    for session in &live_sessions {
        match session.agent.snapshot().await {
            Ok(snapshot) => {
                if let Err(message) = persist_runtime_records(
                    &application,
                    &mut durable,
                    session.id,
                    &snapshot.record,
                ) {
                    report_notice(&mut app, message);
                }
            }
            Err(error) => report_notice(
                &mut app,
                format!("Could not snapshot a closing runtime: {error}"),
            ),
        }
        if let Err(message) = persist_interruption(
            &application,
            &mut durable,
            session.id,
            "BONE exited while this turn was still running",
        ) {
            report_notice(&mut app, message);
        }
    }

    drop(updates);
    let reports = join_all(live_sessions.iter().map(|session| session.agent.shutdown())).await;
    for (session, report) in live_sessions.iter().zip(&reports) {
        if let Ok(report) = report
            && let Err(message) =
                persist_unresolved_shutdown_effects(&application, &mut durable, session.id, report)
        {
            report_notice(&mut app, message);
        }
    }
    for session in &mut live_sessions {
        let _ = (&mut session.observer).await;
    }

    ui_result?;
    reports
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use super::*;
    use crate::tui::session_controller::{
        AcceptedPost, DurableUiSession, PreparedPost, accept_post, activate_writer_session,
        persist_draft, persist_interruption, persist_model_readiness, persist_runtime_attached,
        persist_runtime_receipt, persist_runtime_records, persist_runtime_retryable,
        persist_runtime_starting, persist_status, reconcile_cold_runtime,
        reconcile_journal_summary, release_idle_writer_leases_after_switch,
    };
    use crate::{
        JournalFact, JournalRead, RuntimeAttachment, SessionAttention, SessionAvailability,
        SessionDraft, SessionExecution, SessionRecord, TurnOutcome, WorkspaceApplication,
    };
    use bone_agent::{JobId, JobOutcome, Notice, RecordEntry, RecordKind, TaskConfig};

    const UI_ID: UiSessionId = UiSessionId(1);

    struct Fixture {
        // These directories must stay alive for the duration of each storage
        // assertion. Their names intentionally begin with `_` because the
        // test only needs their lifetime, not their paths after construction.
        _project: tempfile::TempDir,
        _state: tempfile::TempDir,
        application: WorkspaceApplication,
        logical_id: crate::SessionId,
        durable: HashMap<UiSessionId, DurableUiSession>,
    }

    fn private_state() -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(
            directory.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .unwrap();
        directory
    }

    fn test_fixture() -> Fixture {
        let project = tempfile::tempdir().unwrap();
        let state = private_state();
        let application = WorkspaceApplication::open_in(project.path(), state.path()).unwrap();
        let opened = application.open_or_create_writer_draft().unwrap();
        let record = opened.draft.record;
        let writer_lease = opened.lease;
        let journal = application.sessions().journal(record.id).unwrap();
        let mut durable = HashMap::new();
        durable.insert(
            UI_ID,
            DurableUiSession {
                record,
                journal: Some(journal),
                writer_lease: Some(writer_lease),
                next_turn: 1,
                active_turn: None,
                runtime_record_cursor: 0,
            },
        );
        Fixture {
            _project: project,
            _state: state,
            application,
            logical_id: durable.get(&UI_ID).unwrap().record.id,
            durable,
        }
    }

    fn persisted(fixture: &Fixture) -> SessionRecord {
        fixture
            .application
            .sessions()
            .get(fixture.logical_id)
            .unwrap()
            .unwrap()
    }

    fn prepared_post() -> PreparedPost {
        PreparedPost {
            task: TaskConfig {
                model: Some("gpt-test".into()),
                ..TaskConfig::default()
            },
            effective_config_revision: "settings-revision-1".into(),
        }
    }

    fn accept(fixture: &mut Fixture, queue_runtime: bool) -> AcceptedPost {
        accept_post(
            &fixture.application,
            &mut fixture.durable,
            UI_ID,
            "implement the change".into(),
            prepared_post(),
            queue_runtime,
        )
        .unwrap()
    }

    fn record_test_receipt(fixture: &mut Fixture) {
        persist_runtime_receipt(
            &fixture.application,
            &mut fixture.durable,
            UI_ID,
            1,
            "settings-revision-1",
            "gpt-test",
        )
        .unwrap();
    }

    #[test]
    fn durable_acceptance_clears_draft_and_keeps_recovery_attention() {
        let mut fixture = test_fixture();
        let draft =
            SessionDraft::new("implement the change", "implement the change".len()).unwrap();
        persist_draft(&fixture.application, &mut fixture.durable, UI_ID, draft).unwrap();
        persist_status(
            &fixture.application,
            &mut fixture.durable,
            UI_ID,
            |status| {
                status.attention.insert(SessionAttention::ConfigPending);
                status.attention.insert(SessionAttention::RecoveryNeeded);
            },
        )
        .unwrap();

        let accepted = accept(&mut fixture, true);
        assert!(accepted.record_persistence_error.is_none());
        assert_eq!(fixture.durable[&UI_ID].active_turn, Some(1));
        assert_eq!(fixture.durable[&UI_ID].next_turn, 2);

        let record = persisted(&fixture);
        assert_eq!(record.draft, SessionDraft::empty());
        assert_eq!(record.status.execution, SessionExecution::QueuedForRuntime);
        assert_eq!(record.status.attachment, RuntimeAttachment::Detached);
        assert_eq!(record.status.availability, SessionAvailability::Local);
        assert!(
            !record
                .status
                .attention
                .contains(&SessionAttention::ConfigPending)
        );
        assert!(
            record
                .status
                .attention
                .contains(&SessionAttention::RecoveryNeeded)
        );

        let journal = fixture
            .application
            .sessions()
            .journal(fixture.logical_id)
            .unwrap()
            .read()
            .unwrap();
        assert!(matches!(
            journal.entries.as_slice(),
            [entry] if matches!(
                &entry.fact,
                JournalFact::UserTurnAccepted {
                    turn: 1,
                    text,
                    effective_config_revision,
                    solver_model,
                } if text == "implement the change"
                    && effective_config_revision == "settings-revision-1"
                    && solver_model == "gpt-test"
            )
        ));

        // An attached runtime still has not acknowledged this exact message.
        // The durable execution state must remain queued until the receipt.
        let mut attached_fixture = test_fixture();
        let accepted = accept(&mut attached_fixture, false);
        assert!(accepted.record_persistence_error.is_none());
        let record = persisted(&attached_fixture);
        assert_eq!(record.status.execution, SessionExecution::QueuedForRuntime);
        assert_eq!(record.status.attachment, RuntimeAttachment::Attached);
    }

    #[test]
    fn runtime_summary_moves_through_start_attach_receipt_and_retries_stale_cas() {
        let mut fixture = test_fixture();
        accept(&mut fixture, true);

        persist_runtime_starting(&fixture.application, &mut fixture.durable, UI_ID).unwrap();
        let record = persisted(&fixture);
        assert_eq!(record.status.execution, SessionExecution::Opening);
        assert_eq!(record.status.attachment, RuntimeAttachment::Attaching);

        persist_runtime_attached(&fixture.application, &mut fixture.durable, UI_ID, true).unwrap();
        let record = persisted(&fixture);
        assert_eq!(record.status.execution, SessionExecution::Opening);
        assert_eq!(record.status.attachment, RuntimeAttachment::Attached);

        record_test_receipt(&mut fixture);
        let record = persisted(&fixture);
        assert_eq!(record.status.execution, SessionExecution::Working);
        assert_eq!(record.status.attachment, RuntimeAttachment::Attached);

        // A different process can update metadata after this TUI cached the
        // old revision. The next focused mutation reloads, preserves that
        // external change, and retries the status patch once.
        let current = persisted(&fixture);
        let external = fixture
            .application
            .sessions()
            .rename(current.id, current.revision, "Edited elsewhere")
            .unwrap();
        persist_runtime_retryable(&fixture.application, &mut fixture.durable, UI_ID).unwrap();
        let record = persisted(&fixture);
        assert_eq!(record.metadata.title, external.metadata.title);
        assert_eq!(record.status.execution, SessionExecution::QueuedForRuntime);
        assert_eq!(record.status.attachment, RuntimeAttachment::Detached);
        assert_eq!(record.status.availability, SessionAvailability::Local);
        assert_eq!(fixture.durable[&UI_ID].record.revision, record.revision);
    }

    #[test]
    fn terminal_notices_and_unknown_effects_are_persisted_once() {
        let mut fixture = test_fixture();
        accept(&mut fixture, false);
        record_test_receipt(&mut fixture);

        let terminal = RecordEntry {
            cursor: 1,
            kind: RecordKind::Notice(Notice::Finished {
                cleanup: Vec::new(),
            }),
        };
        persist_runtime_records(
            &fixture.application,
            &mut fixture.durable,
            UI_ID,
            std::slice::from_ref(&terminal),
        )
        .unwrap();
        let terminal_record = persisted(&fixture);
        assert_eq!(terminal_record.status.execution, SessionExecution::Complete);
        assert_eq!(
            terminal_record.status.attachment,
            RuntimeAttachment::Attached
        );
        assert_eq!(fixture.durable[&UI_ID].active_turn, None);
        let revision_after_terminal = terminal_record.revision;

        // A reset after a broadcast gap repeats the same record. It must not
        // append a second TurnFinished fact or churn the record revision.
        persist_runtime_records(
            &fixture.application,
            &mut fixture.durable,
            UI_ID,
            std::slice::from_ref(&terminal),
        )
        .unwrap();
        assert_eq!(persisted(&fixture).revision, revision_after_terminal);

        let unknown = RecordEntry {
            cursor: 2,
            kind: RecordKind::Notice(Notice::JobFinished {
                id: JobId(7),
                outcome: JobOutcome::unknown("write may have succeeded"),
            }),
        };
        persist_runtime_records(
            &fixture.application,
            &mut fixture.durable,
            UI_ID,
            std::slice::from_ref(&unknown),
        )
        .unwrap();
        let record = persisted(&fixture);
        assert_eq!(record.status.execution, SessionExecution::Complete);
        assert!(
            record
                .status
                .attention
                .contains(&SessionAttention::UnresolvedEffect)
        );

        let journal = fixture
            .application
            .sessions()
            .journal(fixture.logical_id)
            .unwrap()
            .read()
            .unwrap();
        assert!(matches!(
            journal.entries[1].fact,
            JournalFact::TurnStarted {
                turn: 1,
                ref effective_config_revision,
                ref solver_model,
            } if effective_config_revision == "settings-revision-1" && solver_model == "gpt-test"
        ));
        assert!(matches!(
            journal.entries[2].fact,
            JournalFact::TurnFinished {
                turn: 1,
                outcome: TurnOutcome::Completed,
            }
        ));
        assert!(matches!(
            journal.entries[3].fact,
            JournalFact::UnresolvedExternalEffect { .. }
        ));
    }

    #[test]
    fn interruption_detaches_idle_runtimes_without_forging_an_interruption_fact() {
        let mut fixture = test_fixture();
        accept(&mut fixture, false);
        record_test_receipt(&mut fixture);

        persist_interruption(
            &fixture.application,
            &mut fixture.durable,
            UI_ID,
            "runtime closed",
        )
        .unwrap();
        let record = persisted(&fixture);
        assert_eq!(record.status.execution, SessionExecution::Interrupted);
        assert_eq!(record.status.attachment, RuntimeAttachment::Detached);
        assert_eq!(fixture.durable[&UI_ID].active_turn, None);
        let journal = fixture
            .application
            .sessions()
            .journal(fixture.logical_id)
            .unwrap()
            .read()
            .unwrap();
        assert!(matches!(
            journal.entries.last().unwrap().fact,
            JournalFact::RuntimeInterrupted { .. }
        ));

        let mut idle_fixture = test_fixture();
        persist_status(
            &idle_fixture.application,
            &mut idle_fixture.durable,
            UI_ID,
            |status| {
                status.execution = SessionExecution::Complete;
                status.attachment = RuntimeAttachment::Attached;
            },
        )
        .unwrap();
        persist_interruption(
            &idle_fixture.application,
            &mut idle_fixture.durable,
            UI_ID,
            "idle runtime closed",
        )
        .unwrap();
        let idle_record = persisted(&idle_fixture);
        assert_eq!(idle_record.status.execution, SessionExecution::Complete);
        assert_eq!(idle_record.status.attachment, RuntimeAttachment::Detached);
        assert!(
            idle_fixture
                .application
                .sessions()
                .journal(idle_fixture.logical_id)
                .unwrap()
                .read()
                .unwrap()
                .entries
                .is_empty()
        );
    }

    #[test]
    fn cold_recovery_never_replays_an_unconfirmed_turn_and_repairs_summary_from_history() {
        let mut fixture = test_fixture();
        accept(&mut fixture, true);
        let mut journal = fixture
            .application
            .sessions()
            .journal(fixture.logical_id)
            .unwrap()
            .read()
            .unwrap();

        assert!(
            reconcile_cold_runtime(
                &fixture.application,
                &mut fixture.durable,
                UI_ID,
                &mut journal,
            )
            .unwrap()
        );
        let record = persisted(&fixture);
        assert_eq!(record.status.execution, SessionExecution::Interrupted);
        assert_eq!(record.status.attachment, RuntimeAttachment::Detached);
        assert!(
            record
                .status
                .attention
                .contains(&SessionAttention::RecoveryNeeded)
        );
        assert_eq!(fixture.durable[&UI_ID].active_turn, None);
        assert!(matches!(
            journal.entries.as_slice(),
            [.., entry] if matches!(entry.fact, JournalFact::RuntimeInterrupted { .. })
        ));
        assert!(
            !journal
                .entries
                .iter()
                .any(|entry| matches!(entry.fact, JournalFact::TurnStarted { .. }))
        );

        let mut terminal_fixture = test_fixture();
        accept(&mut terminal_fixture, false);
        record_test_receipt(&mut terminal_fixture);
        let terminal = RecordEntry {
            cursor: 1,
            kind: RecordKind::Notice(Notice::Finished {
                cleanup: Vec::new(),
            }),
        };
        persist_runtime_records(
            &terminal_fixture.application,
            &mut terminal_fixture.durable,
            UI_ID,
            &[terminal],
        )
        .unwrap();
        persist_status(
            &terminal_fixture.application,
            &mut terminal_fixture.durable,
            UI_ID,
            |status| {
                // Simulate the secondary summary write having failed after a
                // prior process durably appended TurnFinished.
                status.execution = SessionExecution::Working;
                status.attachment = RuntimeAttachment::Attached;
            },
        )
        .unwrap();
        let journal = terminal_fixture
            .application
            .sessions()
            .journal(terminal_fixture.logical_id)
            .unwrap()
            .read()
            .unwrap();
        reconcile_journal_summary(
            &terminal_fixture.application,
            &mut terminal_fixture.durable,
            UI_ID,
            &journal,
        )
        .unwrap();
        let record = persisted(&terminal_fixture);
        assert_eq!(record.status.execution, SessionExecution::Complete);
        assert_eq!(record.status.attachment, RuntimeAttachment::Detached);
    }

    #[test]
    fn model_readiness_changes_only_the_configuration_gate() {
        let mut fixture = test_fixture();
        persist_model_readiness(&fixture.application, &mut fixture.durable, UI_ID, false).unwrap();
        let record = persisted(&fixture);
        assert_eq!(record.status.execution, SessionExecution::QueuedForSetup);
        assert!(
            record
                .status
                .attention
                .contains(&SessionAttention::ConfigPending)
        );

        persist_model_readiness(&fixture.application, &mut fixture.durable, UI_ID, true).unwrap();
        let record = persisted(&fixture);
        assert_eq!(record.status.execution, SessionExecution::Ready);
        assert!(
            !record
                .status
                .attention
                .contains(&SessionAttention::ConfigPending)
        );
        let ready_revision = record.revision;
        persist_model_readiness(&fixture.application, &mut fixture.durable, UI_ID, true).unwrap();
        assert_eq!(persisted(&fixture).revision, ready_revision);

        accept(&mut fixture, false);
        record_test_receipt(&mut fixture);
        persist_model_readiness(&fixture.application, &mut fixture.durable, UI_ID, false).unwrap();
        let record = persisted(&fixture);
        assert_eq!(record.status.execution, SessionExecution::Working);
        assert!(
            record
                .status
                .attention
                .contains(&SessionAttention::ConfigPending)
        );
    }

    #[test]
    fn an_unleased_session_cannot_append_a_turn_or_change_its_summary() {
        let mut fixture = test_fixture();
        let before = persisted(&fixture);
        let lease = fixture.durable.get_mut(&UI_ID).unwrap().writer_lease.take();
        drop(lease);

        let draft = SessionDraft::new("must not become a ghost draft", 0).unwrap();
        assert!(persist_draft(&fixture.application, &mut fixture.durable, UI_ID, draft).is_err());
        assert!(
            accept_post(
                &fixture.application,
                &mut fixture.durable,
                UI_ID,
                "must not be accepted".into(),
                prepared_post(),
                true,
            )
            .is_err()
        );
        assert_eq!(persisted(&fixture), before);
        assert!(
            fixture
                .application
                .sessions()
                .journal(fixture.logical_id)
                .unwrap()
                .read()
                .unwrap()
                .entries
                .is_empty()
        );
    }

    #[test]
    fn lazy_writer_activation_reconciles_a_background_active_turn_before_editing() {
        let mut fixture = test_fixture();
        fixture
            .application
            .sessions()
            .journal(fixture.logical_id)
            .unwrap()
            .append(JournalFact::UserTurnAccepted {
                turn: 1,
                text: "inspect the failure".into(),
                effective_config_revision: "cfg-1".into(),
                solver_model: "gpt-test".into(),
            })
            .unwrap();
        let lease = fixture.durable.get_mut(&UI_ID).unwrap().writer_lease.take();
        drop(lease);
        let mut app = App::new("workspace".into());

        assert!(activate_writer_session(
            &fixture.application,
            None,
            &mut fixture.durable,
            &mut app,
            UI_ID,
            false,
        ));
        assert!(fixture.durable[&UI_ID].writer_lease.is_some());
        assert_eq!(fixture.durable[&UI_ID].active_turn, None);
        assert!(
            fixture
                .application
                .sessions()
                .journal(fixture.logical_id)
                .unwrap()
                .read()
                .unwrap()
                .entries
                .iter()
                .any(|entry| matches!(entry.fact, JournalFact::RuntimeInterrupted { .. }))
        );
    }

    #[test]
    fn successful_session_switch_releases_only_idle_writer_leases() {
        let mut fixture = test_fixture();
        let second_record = fixture.application.sessions().create("Second").unwrap();
        let second_journal = fixture
            .application
            .sessions()
            .journal(second_record.id)
            .unwrap();
        let second_lease = fixture
            .application
            .sessions()
            .try_acquire_writer_lease(second_record.id)
            .unwrap();
        let second_id = UiSessionId(2);
        fixture.durable.insert(
            second_id,
            DurableUiSession {
                record: second_record.clone(),
                journal: Some(second_journal),
                writer_lease: Some(second_lease),
                next_turn: 1,
                active_turn: None,
                runtime_record_cursor: 0,
            },
        );
        let first_record = fixture.durable[&UI_ID].record.clone();
        let empty = JournalRead::default();
        let mut app = App::new("workspace".into());
        let _ = app.reduce(AppEvent::SessionHydrated {
            id: UI_ID,
            record: &first_record,
            journal: &empty,
            show_progress: true,
            ready_to_attach: true,
            writer_available: true,
            select: true,
        });
        let _ = app.reduce(AppEvent::SessionHydrated {
            id: second_id,
            record: &second_record,
            journal: &empty,
            show_progress: true,
            ready_to_attach: true,
            writer_available: true,
            select: false,
        });
        let _ = app.reduce(AppEvent::SessionSelected { id: second_id });
        let live_sessions = Vec::new();
        let pending_tasks = HashMap::new();
        let starting_ids = HashSet::new();
        let busy_ids = busy_session_ids(&live_sessions, &pending_tasks, &starting_ids);

        release_idle_writer_leases_after_switch(&mut fixture.durable, &mut app, &busy_ids);

        assert!(fixture.durable[&UI_ID].writer_lease.is_none());
        assert!(fixture.durable[&second_id].writer_lease.is_some());
        assert!(matches!(
            app.sessions[0].state,
            crate::tui::app::SessionState::ReadOnlyElsewhere(_)
        ));
    }
}
