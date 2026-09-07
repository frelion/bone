//! Durable Session control for the product TUI.
//!
//! This module owns only facts that survive the current BONE process:
//! SessionRecord mutations, writers, journals, recovery, and the
//! accepted-turn boundary. It deliberately knows nothing about terminal
//! polling or Agent runtime handles.

use std::collections::{HashMap, HashSet};

use crate::{
    JournalFact, JournalRead, ModelResolution, ResolvedRuntime, RuntimeAttachment,
    SessionAttention, SessionAvailability, SessionDraft, SessionExecution, SessionJournal,
    SessionLeaseError, SessionLifecycle, SessionRecord, SessionStatus, SessionWriter,
    SettingsService, TurnOutcome, WorkspaceApplication,
};
use bone_agent::{RecordEntry, ShutdownReport};

use super::{
    app::{App, AppEvent, UiSessionId},
    report_notice,
};

pub(super) struct DurableUiSession {
    pub(super) record: SessionRecord,
    pub(super) journal: Option<SessionJournal>,
    /// Long-lived writer for every durable write or attached runtime belonging
    /// to this logical session. A missing writer means this
    /// TUI is strictly read-only for it, even though its history stays visible.
    pub(super) writer: Option<SessionWriter>,
    pub(super) next_turn: u64,
    /// The active durable turn is intentionally singular for now. The Agent
    /// runtime can receive messages while working, but its terminal notices do
    /// not carry a turn ID. The product layer therefore serializes accepted
    /// turns until that protocol grows explicit correlation.
    pub(super) active_turn: Option<u64>,
    /// Record cursors belong to one attached runtime and restart at one after
    /// a reattach. This lets Step and lag-recovery Reset share an idempotent
    /// persistence path without duplicating journal facts.
    pub(super) runtime_record_cursor: u64,
}

pub(super) struct PreparedPost {
    pub(super) runtime: ResolvedRuntime,
    pub(super) runtime_fingerprint: String,
}

impl PreparedPost {
    /// Freeze the exact runtime configuration that will receive the accepted
    /// turn. This is also used for an already-attached runtime: its adapter
    /// cannot hot-swap after `/model`, so subsequent turns must retain the
    /// old runtime's truthful attribution until it is recreated.
    pub(super) fn for_runtime(runtime: ResolvedRuntime) -> Self {
        let runtime_fingerprint = runtime.fingerprint();
        Self {
            runtime,
            runtime_fingerprint,
        }
    }
}

/// The immutable execution choice for a durable turn awaiting a runtime
/// receipt. Text belongs to the journal/UI pending-post projection; this
/// record carries only what the runtime start and receipt boundary need.
#[derive(Clone)]
pub(super) struct PendingRuntimeTurn {
    pub(super) turn: u64,
    pub(super) runtime: ResolvedRuntime,
    pub(super) runtime_fingerprint: String,
    pub(super) solver_model: String,
}

pub(super) struct AcceptedPost {
    pub(super) runtime: ResolvedRuntime,
    pub(super) text: String,
    pub(super) entry: crate::JournalEntry,
    pub(super) turn: u64,
    pub(super) runtime_fingerprint: String,
    pub(super) solver_model: String,
}

impl AcceptedPost {
    pub(super) fn pending_runtime_turn(&self) -> PendingRuntimeTurn {
        PendingRuntimeTurn {
            turn: self.turn,
            runtime: self.runtime.clone(),
            runtime_fingerprint: self.runtime_fingerprint.clone(),
            solver_model: self.solver_model.clone(),
        }
    }
}

pub(super) fn require_writer(session: &DurableUiSession) -> Result<(), String> {
    if session.writer.is_some() {
        Ok(())
    } else {
        Err(format!(
            "Conversation {} is open for writing in another BONE process",
            session.record.id
        ))
    }
}

/// Change one logical session's durable summary through its owned writer.
/// Runtime handles stay outside this record; only the last known
/// execution/attachment truth is persisted.
fn mutate_durable_record(
    durable: &mut HashMap<UiSessionId, DurableUiSession>,
    id: UiSessionId,
    mutate: impl Fn(&mut SessionRecord),
) -> Result<(), String> {
    let session = durable
        .get_mut(&id)
        .ok_or_else(|| "This conversation is not backed by durable storage".to_owned())?;
    require_writer(session)?;
    if session.record.status.lifecycle != SessionLifecycle::Active {
        return Err("This conversation is no longer active".to_owned());
    }
    let mut record = session.record.clone();
    mutate(&mut record);
    // A replayed observer reset or an unchanged model-readiness refresh is
    // not a state transition, so avoid needless durable writes.
    if record == session.record {
        return Ok(());
    }
    let writer = session.writer.as_mut().expect("writer was required");
    writer.replace(record).map_err(|error| error.to_string())?;
    session.record = writer.record().clone();
    Ok(())
}

pub(super) fn persist_status(
    durable: &mut HashMap<UiSessionId, DurableUiSession>,
    id: UiSessionId,
    mutate: impl Fn(&mut SessionStatus),
) -> Result<(), String> {
    mutate_durable_record(durable, id, |record| {
        mutate(&mut record.status);
    })
}

/// A saved turn whose runtime has not acknowledged receipt remains retryable.
/// Connection failures are not local-storage failures, so availability stays
/// Local rather than borrowing the overloaded UI word offline.
pub(super) fn persist_runtime_retryable(
    _application: &WorkspaceApplication,
    durable: &mut HashMap<UiSessionId, DurableUiSession>,
    id: UiSessionId,
) -> Result<(), String> {
    persist_status(durable, id, |status| {
        status.execution = SessionExecution::QueuedForRuntime;
        status.attachment = RuntimeAttachment::Detached;
        status.availability = SessionAvailability::Local;
    })
}

/// The runtime start effect has been scheduled but has not yet produced an
/// observed Agent handle.
pub(super) fn persist_runtime_starting(
    _application: &WorkspaceApplication,
    durable: &mut HashMap<UiSessionId, DurableUiSession>,
    id: UiSessionId,
) -> Result<(), String> {
    persist_status(durable, id, |status| {
        status.execution = SessionExecution::Opening;
        status.attachment = RuntimeAttachment::Attaching;
        status.availability = SessionAvailability::Local;
    })
}

/// An Agent runtime exists and can be observed. It may still be waiting for
/// the durable turn to receive a runtime receipt.
pub(super) fn persist_runtime_attached(
    _application: &WorkspaceApplication,
    durable: &mut HashMap<UiSessionId, DurableUiSession>,
    id: UiSessionId,
    awaiting_receipt: bool,
) -> Result<(), String> {
    persist_status(durable, id, |status| {
        status.execution = if awaiting_receipt {
            SessionExecution::Opening
        } else {
            SessionExecution::Ready
        };
        status.attachment = RuntimeAttachment::Attached;
        status.availability = SessionAvailability::Local;
    })
}

/// AgentHandle::post has returned a receipt for the durable turn. Only now
/// may the summary say that runtime work has begun.
pub(super) fn persist_runtime_receipt(
    _application: &WorkspaceApplication,
    durable: &mut HashMap<UiSessionId, DurableUiSession>,
    id: UiSessionId,
    turn: u64,
    runtime_fingerprint: &str,
    solver_model: &str,
) -> Result<(), String> {
    let session = durable
        .get_mut(&id)
        .ok_or_else(|| "This conversation is not backed by durable storage".to_owned())?;
    require_writer(session)?;
    if session.active_turn != Some(turn) {
        return Err("The runtime receipt no longer matches this conversation's active turn".into());
    }
    session.journal.as_ref().ok_or_else(|| {
        "Conversation history is unavailable; the runtime receipt was not recorded".to_owned()
    })?;
    session
        .writer
        .as_mut()
        .expect("writer was required")
        .append(JournalFact::TurnStarted {
            turn,
            runtime_fingerprint: runtime_fingerprint.to_owned(),
            solver_model: solver_model.to_owned(),
        })
        .map_err(|error| format!("Could not save runtime receipt: {error}"))?;
    persist_status(durable, id, |status| {
        status.execution = SessionExecution::Working;
        status.attachment = RuntimeAttachment::Attached;
        status.availability = SessionAvailability::Local;
    })
}

pub(super) fn persist_runtime_stopping(
    _application: &WorkspaceApplication,
    durable: &mut HashMap<UiSessionId, DurableUiSession>,
    id: UiSessionId,
) -> Result<(), String> {
    persist_status(durable, id, |status| {
        status.execution = SessionExecution::Stopping;
        status.attachment = RuntimeAttachment::Attached;
        status.availability = SessionAvailability::Local;
    })
}

/// Model readiness is a configuration gate, not a runtime receipt. Do not
/// rewrite an already accepted/working turn just because its inherited model
/// selection changed; the next normal turn will resolve the new setting.
pub(super) fn persist_model_readiness(
    _application: &WorkspaceApplication,
    durable: &mut HashMap<UiSessionId, DurableUiSession>,
    id: UiSessionId,
    ready: bool,
) -> Result<(), String> {
    persist_status(durable, id, |status| {
        if ready {
            status.attention.remove(&SessionAttention::ConfigPending);
            if matches!(
                status.execution,
                SessionExecution::Draft
                    | SessionExecution::QueuedForSetup
                    | SessionExecution::Offline
            ) {
                status.execution = SessionExecution::Ready;
                status.attachment = RuntimeAttachment::Detached;
                status.availability = SessionAvailability::Local;
            }
        } else {
            status.attention.insert(SessionAttention::ConfigPending);
            if matches!(
                status.execution,
                SessionExecution::Draft
                    | SessionExecution::QueuedForSetup
                    | SessionExecution::Ready
                    | SessionExecution::Offline
            ) {
                status.execution = SessionExecution::QueuedForSetup;
                status.attachment = RuntimeAttachment::Detached;
                status.availability = SessionAvailability::Local;
            }
        }
    })
}

/// Project terminal Agent notices only after their journal facts have been
/// appended. A summary is deliberately secondary to that append-only history.
fn persist_runtime_observation_status(
    _application: &WorkspaceApplication,
    durable: &mut HashMap<UiSessionId, DurableUiSession>,
    id: UiSessionId,
    terminal: Option<SessionExecution>,
    unresolved_effect: bool,
) -> Result<(), String> {
    if terminal.is_none() && !unresolved_effect {
        return Ok(());
    }
    persist_status(durable, id, |status| {
        if let Some(execution) = terminal {
            status.execution = execution;
            status.attachment = RuntimeAttachment::Attached;
            status.availability = SessionAvailability::Local;
        }
        if unresolved_effect {
            status.attention.insert(SessionAttention::UnresolvedEffect);
        }
    })
}

pub(super) fn model_is_ready(settings: &SettingsService, record: &SessionRecord) -> bool {
    settings
        .resolve_model(record)
        .ok()
        .is_some_and(|resolution| matches!(resolution, ModelResolution::Ready(_)))
}

/// Lazily acquire a session writer, then rerun the same cold-start
/// reconciliation that the initially selected conversation receives. A
/// background session is deliberately never recovered or status-mutated until
/// this process owns it: otherwise opening BONE would write every visible
/// conversation and contend with another BONE instance.
pub(super) fn activate_writer_session(
    application: &WorkspaceApplication,
    settings: &SettingsService,
    durable: &mut HashMap<UiSessionId, DurableUiSession>,
    app: &mut App,
    id: UiSessionId,
    force_refresh: bool,
) -> bool {
    let Some(logical_id) = durable.get(&id).map(|session| session.record.id) else {
        report_notice(app, "This conversation is not backed by durable storage");
        return false;
    };
    let already_owned = durable
        .get(&id)
        .is_some_and(|session| session.writer.is_some());
    if already_owned && !force_refresh {
        return true;
    }
    if !already_owned {
        let writer = match application.sessions().try_open_writer(logical_id) {
            Ok(writer) => writer,
            Err(SessionLeaseError::HeldElsewhere { .. }) => {
                let _ = app.reduce(AppEvent::SessionReadOnlyElsewhere {
                    id,
                    message:
                        "This conversation is open in another BONE process and is read-only here"
                            .into(),
                });
                return false;
            }
            Err(error) => {
                let _ = app.reduce(AppEvent::SessionReadOnlyElsewhere {
                    id,
                    message: format!("Could not acquire editing access: {error}"),
                });
                return false;
            }
        };
        if let Some(session) = durable.get_mut(&id) {
            session.writer = Some(writer);
        }
    }

    // The writer reads its record only after it owns the lock, so it is the
    // authoritative local copy for the following recovery work.
    let refreshed_record = durable
        .get(&id)
        .and_then(|session| session.writer.as_ref())
        .map(|writer| writer.record().clone());
    let Some(refreshed_record) = refreshed_record else {
        return false;
    };

    let (journal, mut journal_read, journal_problem) =
        match application.sessions().journal(logical_id) {
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
    let Some(session) = durable.get_mut(&id) else {
        return false;
    };
    session.record = refreshed_record;
    session.journal = journal;
    let (next_turn, active_turn) = turn_state(&journal_read);
    session.next_turn = next_turn;
    session.active_turn = active_turn;
    session.runtime_record_cursor = 0;

    let ready_to_attach = durable
        .get(&id)
        .is_some_and(|session| model_is_ready(settings, &session.record));
    let mut notices = Vec::new();
    let delivery_unconfirmed = if journal_problem.is_none() {
        match reconcile_cold_runtime(application, durable, id, &mut journal_read) {
            Ok(unconfirmed) => unconfirmed,
            Err(error) => {
                notices.push(format!(
                    "Could not save interrupted work during recovery: {error}"
                ));
                false
            }
        }
    } else {
        false
    };
    let journal_needs_recovery = journal_problem.is_some() || delivery_unconfirmed;
    if journal_problem.is_none()
        && let Err(error) = reconcile_journal_summary(application, durable, id, &journal_read)
    {
        notices.push(format!(
            "Could not reconcile this conversation's saved status: {error}"
        ));
    }
    if let Err(error) = persist_model_readiness(application, durable, id, ready_to_attach) {
        notices.push(format!(
            "Could not update this conversation's model readiness: {error}"
        ));
    }
    if journal_needs_recovery
        && let Err(error) = persist_status(durable, id, |status| {
            status.attention.insert(SessionAttention::RecoveryNeeded);
        })
    {
        notices.push(format!(
            "Could not flag this conversation for history recovery: {error}"
        ));
    }

    if let Some(session) = durable.get(&id) {
        let _ = app.reduce(AppEvent::SessionWriterAcquired {
            id,
            record: &session.record,
            journal: &journal_read,
            ready_to_attach,
        });
    }
    if let Some(problem) = journal_problem {
        let _ = app.reduce(AppEvent::SessionNeedsSetup {
            id,
            message: problem,
        });
    }
    for notice in notices {
        report_notice(app, notice);
    }
    true
}

/// Release local ownership that is no longer needed after a successful
/// session switch. Runtime/pending/active-turn sessions keep their writers,
/// because their observer, receipt, and shutdown paths still append durable
/// facts. If acquiring the newly selected session failed, callers must not
/// invoke this helper: preserving the prior lease lets the user return to a
/// known-writable conversation instead of losing it to a race.
pub(super) fn release_idle_writers_after_switch(
    durable: &mut HashMap<UiSessionId, DurableUiSession>,
    app: &mut App,
    busy_ids: &HashSet<UiSessionId>,
) {
    let selected = app.current_id();
    let released = durable
        .iter_mut()
        .filter_map(|(id, session)| {
            let retain = *id == selected || busy_ids.contains(id) || session.active_turn.is_some();
            if !retain && session.writer.take().is_some() {
                Some(*id)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    for id in released {
        let _ = app.reduce(AppEvent::SessionReadOnlyElsewhere {
            id,
            message: "Select this conversation to request editing access".into(),
        });
    }
}

pub(super) fn persist_draft(
    _application: &WorkspaceApplication,
    durable: &mut HashMap<UiSessionId, DurableUiSession>,
    id: UiSessionId,
    draft: SessionDraft,
) -> Result<(), String> {
    mutate_durable_record(durable, id, |record| {
        record.draft = draft.clone();
    })
}

pub(super) fn prepare_post(
    settings: &SettingsService,
    durable: &HashMap<UiSessionId, DurableUiSession>,
    id: UiSessionId,
) -> Result<PreparedPost, String> {
    let session = durable
        .get(&id)
        .ok_or_else(|| "This conversation is not backed by durable storage".to_owned())?;
    let resolution = settings
        .resolve_model(&session.record)
        .map_err(|error| error.to_string())?;
    let runtime = resolution
        .runtime()
        .cloned()
        .ok_or_else(|| "Choose a model with /model <id> before sending work".to_owned())?;
    Ok(PreparedPost::for_runtime(runtime))
}

/// Commit one complete durable turn acceptance before the UI changes or an
/// Agent receives anything. It intentionally does not deliver the message;
/// delivery is a separate retryable effect boundary.
pub(super) fn accept_post(
    _application: &WorkspaceApplication,
    durable: &mut HashMap<UiSessionId, DurableUiSession>,
    id: UiSessionId,
    text: String,
    prepared: PreparedPost,
    queue_runtime: bool,
) -> Result<AcceptedPost, String> {
    let (entry, turn, solver_model) = {
        let session = durable
            .get_mut(&id)
            .ok_or_else(|| "This conversation is not backed by durable storage".to_owned())?;
        require_writer(session)?;
        let _journal = session.journal.as_ref().ok_or_else(|| {
            "Conversation history is unavailable; BONE will not risk losing this message".to_owned()
        })?;
        let turn = session.next_turn;
        let solver_model = prepared.runtime.solver.selection.model.clone();
        let mut record = session.record.clone();
        record.draft = SessionDraft::empty();
        // Durable acceptance is not a runtime receipt. Even an attached agent
        // has not necessarily accepted this exact text yet.
        record.status.execution = SessionExecution::QueuedForRuntime;
        record.status.attachment = if queue_runtime {
            RuntimeAttachment::Detached
        } else {
            RuntimeAttachment::Attached
        };
        record.status.availability = SessionAvailability::Local;
        record
            .status
            .attention
            .remove(&SessionAttention::ConfigPending);
        let entry = session
            .writer
            .as_mut()
            .expect("writer was required")
            .accept_turn(
                record,
                JournalFact::UserTurnAccepted {
                    turn,
                    text: text.clone(),
                    runtime_fingerprint: prepared.runtime_fingerprint.clone(),
                    solver_model: solver_model.clone(),
                },
            )
            .map_err(|error| {
                format!("message was not accepted because its turn could not be saved: {error}")
            })?;
        // The session summary and journal fact committed together. Only now
        // may this local projection advance or the reducer clear composer.
        session.record = session
            .writer
            .as_ref()
            .expect("writer was required")
            .record()
            .clone();
        session.active_turn = Some(turn);
        session.next_turn = session.next_turn.saturating_add(1);
        (entry, turn, solver_model)
    };
    Ok(AcceptedPost {
        runtime: prepared.runtime,
        text,
        entry,
        turn,
        runtime_fingerprint: prepared.runtime_fingerprint,
        solver_model,
    })
}

pub(super) fn create_session(
    application: &WorkspaceApplication,
    durable: &mut HashMap<UiSessionId, DurableUiSession>,
    app: &mut App,
    show_progress: bool,
    next_ui_id: &mut u64,
    settings: &SettingsService,
) {
    match application.sessions().create_writer("New conversation") {
        Ok(writer) => {
            let record = writer.record().clone();
            let id = UiSessionId(*next_ui_id);
            *next_ui_id = next_ui_id.saturating_add(1);
            match application.sessions().journal(record.id) {
                Ok(journal) => {
                    let ready = model_is_ready(settings, &record);
                    let empty_journal = JournalRead::default();
                    durable.insert(
                        id,
                        DurableUiSession {
                            record,
                            journal: Some(journal),
                            writer: Some(writer),
                            next_turn: 1,
                            active_turn: None,
                            runtime_record_cursor: 0,
                        },
                    );
                    if let Err(error) = persist_model_readiness(application, durable, id, ready) {
                        report_notice(
                            app,
                            format!("Could not save this conversation's model readiness: {error}"),
                        );
                    }
                    let record = &durable
                        .get(&id)
                        .expect("a newly created durable session was just inserted")
                        .record;
                    let _ = app.reduce(AppEvent::SessionHydrated {
                        id,
                        record,
                        journal: &empty_journal,
                        show_progress,
                        ready_to_attach: ready,
                        writer_available: true,
                        select: true,
                    });
                }
                Err(error) => report_notice(
                    app,
                    format!("Conversation was created but history failed to open: {error}"),
                ),
            }
        }
        Err(error) => report_notice(app, format!("Could not create a conversation: {error}")),
    }
}

/// Persist only runtime records not already accounted for by this attachment.
/// Step and Reset both call this function: a broadcast gap therefore cannot
/// make recovered replies visible only until the next restart.
pub(super) fn persist_runtime_records(
    _application: &WorkspaceApplication,
    durable: &mut HashMap<UiSessionId, DurableUiSession>,
    id: UiSessionId,
    records: &[RecordEntry],
) -> Result<(), String> {
    // First append the journal facts and advance this attachment's cursor.
    // persist_status needs another mutable borrow of durable, so collect
    // the summary transition while the session borrow is live and apply it
    // only after that borrow ends.
    let mut terminal = None;
    let mut unresolved_effect = false;
    let mut journal_error = None;
    {
        let Some(session) = durable.get_mut(&id) else {
            return Ok(());
        };
        require_writer(session)?;
        if session.journal.is_none() {
            return Ok(());
        }
        for record in records {
            if record.cursor <= session.runtime_record_cursor {
                continue;
            }

            let terminal_notice = match &record.kind {
                bone_agent::RecordKind::Notice(bone_agent::Notice::Paused) => Some((
                    TurnOutcome::WaitingForUser,
                    SessionExecution::WaitingForUser,
                )),
                bone_agent::RecordKind::Notice(bone_agent::Notice::Stopped) => {
                    Some((TurnOutcome::Stopped, SessionExecution::Ready))
                }
                bone_agent::RecordKind::Notice(bone_agent::Notice::Finished { .. }) => {
                    Some((TurnOutcome::Completed, SessionExecution::Complete))
                }
                _ => None,
            };
            let is_unresolved_effect = matches!(
                &record.kind,
                bone_agent::RecordKind::Notice(bone_agent::Notice::JobFinished { outcome, .. })
                    if outcome.external_effect == bone_agent::ExternalEffect::Unknown
            );
            let fact = match &record.kind {
                bone_agent::RecordKind::Notice(bone_agent::Notice::Reply { text, .. }) => {
                    Some(JournalFact::AssistantReply { text: text.clone() })
                }
                _ if let Some((outcome, _)) = terminal_notice => session
                    .active_turn
                    .map(|turn| JournalFact::TurnFinished { turn, outcome }),
                _ if is_unresolved_effect => Some(JournalFact::UnresolvedExternalEffect {
                    summary: "an external tool outcome could not be confirmed".into(),
                }),
                _ => None,
            };
            if let Some(fact) = fact
                && let Err(error) = session
                    .writer
                    .as_mut()
                    .expect("writer was required")
                    .append(fact)
            {
                journal_error = Some(format!(
                    "Live output is visible but history was not saved: {error}"
                ));
                break;
            }

            session.runtime_record_cursor = record.cursor;
            if let Some((_, execution)) = terminal_notice
                && session.active_turn.is_some()
            {
                session.active_turn = None;
                terminal = Some(execution);
            }
            if is_unresolved_effect {
                unresolved_effect = true;
            }
        }
    }

    if let Some(error) = journal_error {
        // Successful earlier records still receive their summary projection
        // below. The failed record keeps its cursor, so a future reset can
        // safely retry its journal append rather than duplicating a fact.
        if let Err(status_error) = persist_runtime_observation_status(
            _application,
            durable,
            id,
            terminal,
            unresolved_effect,
        ) {
            return Err(format!(
                "{error}; earlier saved runtime state could not update its durable summary: {status_error}"
            ));
        }
        return Err(error);
    }

    persist_runtime_observation_status(_application, durable, id, terminal, unresolved_effect)
        .map_err(|error| {
            format!(
                "Live output is saved, but its durable session summary could not be updated: {error}"
            )
        })
}

pub(super) fn persist_interruption(
    _application: &WorkspaceApplication,
    durable: &mut HashMap<UiSessionId, DurableUiSession>,
    id: UiSessionId,
    reason: &str,
) -> Result<(), String> {
    let Some(session) = durable.get_mut(&id) else {
        return Ok(());
    };
    require_writer(session)?;
    let has_journal = session.journal.is_some();
    let had_active_turn = session.active_turn.is_some();

    // A completed/waiting turn has already written its terminal fact. Do not
    // add a false interruption merely because an otherwise idle runtime exits,
    // but always detach the durable summary because the process-local handle
    // has definitely gone away.
    let mut interruption_error = None;
    let interrupted = if had_active_turn {
        if has_journal {
            match session
                .writer
                .as_mut()
                .expect("writer was required")
                .append(JournalFact::RuntimeInterrupted {
                    reason: reason.to_owned(),
                }) {
                Ok(_) => {
                    session.active_turn = None;
                    true
                }
                Err(error) => {
                    interruption_error =
                        Some(format!("Could not save runtime interruption: {error}"));
                    false
                }
            }
        } else {
            false
        }
    } else {
        false
    };

    let summary = persist_status(durable, id, |status| {
        status.attachment = RuntimeAttachment::Detached;
        status.availability = SessionAvailability::Local;
        if interrupted {
            status.execution = SessionExecution::Interrupted;
        }
    });
    if let Some(error) = interruption_error {
        return match summary {
            Ok(()) => Err(error),
            Err(status_error) => Err(format!(
                "{error}; runtime detachment could not be recorded either: {status_error}"
            )),
        };
    }
    summary.map_err(|error| format!("Could not record runtime detachment: {error}"))
}

pub(super) fn persist_unresolved_shutdown_effects(
    _application: &WorkspaceApplication,
    durable: &mut HashMap<UiSessionId, DurableUiSession>,
    id: UiSessionId,
    report: &ShutdownReport,
) -> Result<(), String> {
    let Some(session) = durable.get_mut(&id) else {
        return Ok(());
    };
    require_writer(session)?;
    if session.journal.is_none() {
        return Ok(());
    }
    let mut wrote_unresolved_effect = false;
    for job in report
        .unresolved_jobs
        .iter()
        .filter(|job| job.external_write)
    {
        if let Err(error) = session
            .writer
            .as_mut()
            .expect("writer was required")
            .append(JournalFact::UnresolvedExternalEffect {
                summary: format!(
                    "external job {} was unresolved when BONE shut down",
                    job.id.0
                ),
            })
        {
            if wrote_unresolved_effect
                && let Err(status_error) =
                    persist_runtime_observation_status(_application, durable, id, None, true)
            {
                return Err(format!(
                    "Could not save unresolved external effect: {error}; earlier unresolved effects could not update the durable summary: {status_error}"
                ));
            }
            return Err(format!(
                "Could not save unresolved external effect: {error}"
            ));
        }
        wrote_unresolved_effect = true;
    }
    persist_runtime_observation_status(_application, durable, id, None, wrote_unresolved_effect)
        .map_err(|error| format!("Could not flag unresolved external effect: {error}"))
}

pub(super) fn turn_state(journal: &JournalRead) -> (u64, Option<u64>) {
    let mut next_turn = 1_u64;
    let mut active_turn = None;
    for entry in &journal.entries {
        match &entry.fact {
            JournalFact::UserTurnAccepted { turn, .. } | JournalFact::TurnStarted { turn, .. } => {
                next_turn = next_turn.max(turn.saturating_add(1));
                active_turn = Some(*turn);
            }
            JournalFact::TurnFinished { turn, .. } => {
                next_turn = next_turn.max(turn.saturating_add(1));
                if active_turn == Some(*turn) {
                    active_turn = None;
                }
            }
            JournalFact::RuntimeInterrupted { .. } => active_turn = None,
            _ => {}
        }
    }
    (next_turn, active_turn)
}

/// The runtime receipt is an explicit journal boundary. A turn that has one
/// must never be redelivered after a cold start; it is instead converted to
/// the ordinary interrupted recovery boundary.
fn has_active_runtime_receipt(journal: &JournalRead) -> bool {
    let mut active = None::<(u64, bool)>;
    for entry in &journal.entries {
        match &entry.fact {
            JournalFact::UserTurnAccepted { turn, .. } => active = Some((*turn, false)),
            JournalFact::TurnStarted { turn, .. } => {
                if let Some((active_turn, _)) = active
                    && active_turn == *turn
                {
                    active = Some((*turn, true));
                }
            }
            JournalFact::TurnFinished { turn, .. } => {
                if active.is_some_and(|(active_turn, _)| active_turn == *turn) {
                    active = None;
                }
            }
            JournalFact::RuntimeInterrupted { .. } => active = None,
            _ => {}
        }
    }
    active.is_some_and(|(_, received)| received)
}

/// No runtime handle survives a process restart. Reconcile any active journal
/// turn into an explicit interruption before the session is shown again.
///
/// A missing TurnStarted is deliberately not treated as proof that the
/// runtime never received the message: the process may have crashed between
/// AgentHandle::post and the journal append. We therefore do not replay it
/// automatically; the recovery flag tells the user the delivery was not
/// confirmable without risking a duplicate model/tool action.
pub(super) fn reconcile_cold_runtime(
    _application: &WorkspaceApplication,
    durable: &mut HashMap<UiSessionId, DurableUiSession>,
    id: UiSessionId,
    journal_read: &mut JournalRead,
) -> Result<bool, String> {
    if turn_state(journal_read).1.is_none() {
        return Ok(false);
    }
    let delivery_unconfirmed = !has_active_runtime_receipt(journal_read);
    let interruption = persist_interruption(
        _application,
        durable,
        id,
        "BONE was restarted before this turn finished; delivery could not be confirmed",
    );

    // Reload even when the summary write failed: the journal append may have
    // succeeded first, and the in-memory active-turn gate must follow the
    // durable history rather than keep the conversation falsely blocked.
    let refreshed = durable
        .get(&id)
        .and_then(|session| session.journal.as_ref())
        .ok_or_else(|| "Conversation history is unavailable during recovery".to_owned())?
        .read()
        .map_err(|error| format!("Could not reload recovered conversation history: {error}"))?;
    let (next_turn, active_turn) = turn_state(&refreshed);
    if let Some(session) = durable.get_mut(&id) {
        session.next_turn = next_turn;
        session.active_turn = active_turn;
    }
    *journal_read = refreshed;

    interruption?;
    if delivery_unconfirmed {
        persist_status(durable, id, |status| {
            status.attention.insert(SessionAttention::RecoveryNeeded);
        })
        .map_err(|error| format!("Could not flag unconfirmed delivery for recovery: {error}"))?;
    }
    Ok(delivery_unconfirmed)
}

/// Rebuild the small restart summary from durable journal boundaries. This is
/// intentionally one-way: journal facts remain authoritative, while
/// SessionRecord.status can be repaired after a prior disk/CAS failure
/// without re-appending a terminal fact or replaying a runtime record.
pub(super) fn reconcile_journal_summary(
    _application: &WorkspaceApplication,
    durable: &mut HashMap<UiSessionId, DurableUiSession>,
    id: UiSessionId,
    journal: &JournalRead,
) -> Result<(), String> {
    let mut execution = None;
    let mut unresolved_effect = false;
    for entry in &journal.entries {
        match &entry.fact {
            JournalFact::TurnFinished { outcome, .. } => {
                execution = Some(match outcome {
                    TurnOutcome::Completed => SessionExecution::Complete,
                    TurnOutcome::WaitingForUser => SessionExecution::WaitingForUser,
                    TurnOutcome::Stopped => SessionExecution::Ready,
                    TurnOutcome::Failed => SessionExecution::Interrupted,
                });
            }
            JournalFact::RuntimeInterrupted { .. } => {
                execution = Some(SessionExecution::Interrupted);
            }
            JournalFact::UnresolvedExternalEffect { .. } => unresolved_effect = true,
            _ => {}
        }
    }
    if execution.is_none() && !unresolved_effect {
        return Ok(());
    }
    persist_status(durable, id, |status| {
        if let Some(execution) = execution {
            status.execution = execution;
            // Every product startup begins without an in-memory runtime.
            status.attachment = RuntimeAttachment::Detached;
            status.availability = SessionAvailability::Local;
        }
        if unresolved_effect {
            status.attention.insert(SessionAttention::UnresolvedEffect);
        }
    })
}
