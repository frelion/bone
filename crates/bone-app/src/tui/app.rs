use crate::{
    JournalEntry, JournalFact, JournalRead, SessionDraft, SessionRecord,
    SessionStatus as DurableSessionStatus,
};
use bone_agent::{RecordEntry, RecordKind, Snapshot, StepEvent};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::style::Style;
use ratatui_textarea::{CursorMove, TextArea, WrapMode};

use super::{
    agent_projection::{
        Projection, SessionStatus, Speaker, TimelineItem, TimelineKind, Tone, is_finished_notice,
    },
    commands::{InputProvenance, LocalCommand, Submission, parse_submission},
};

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub(crate) struct UiSessionId(pub(crate) u64);

/// The connection state as seen by the workbench. This is intentionally a
/// presentation state rather than an `AgentHost`: the host belongs to the
/// effect executor, while the reducer only records what the user can safely
/// infer about it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum ConnectionState {
    #[default]
    Disconnected,
    Connecting,
    Connected,
    Failed(String),
}

/// Whether the draft currently visible in the composer has made it into the
/// durable SessionRecord. Keeping this in UI state lets an async persistence
/// failure be represented honestly instead of becoming an out-of-band toast.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum DraftPersistenceState {
    #[default]
    Idle,
    Pending,
    Saved,
    Failed(String),
}

/// Every mutation of presentation state enters through this reducer input.
/// Terminal input, live Agent observations, and operation failures no longer
/// mutate the UI from unrelated async tasks. The outer shell executes the
/// resulting `Action` and feeds its outcome back as another `AppEvent`.
pub(crate) enum AppEvent<'a> {
    Terminal(Event),
    RuntimeStep {
        id: UiSessionId,
        step: &'a StepEvent,
    },
    RuntimeReset {
        id: UiSessionId,
        snapshot: &'a Snapshot,
    },
    RuntimeClosed {
        id: UiSessionId,
        reason: String,
    },
    /// Runtime creation or delivery failed before the saved message received a
    /// runtime receipt. Keep the message pending so the effect layer can offer
    /// an explicit retry without pretending it was executed.
    RuntimeStartFailed {
        id: UiSessionId,
        reason: String,
    },
    /// A draft persistence effect completed. The submitted draft is carried
    /// back so a late result cannot overwrite the state of newer typing.
    DraftPersisted {
        id: UiSessionId,
        draft: SessionDraft,
    },
    /// A draft persistence effect failed. The composer remains editable; the
    /// reducer records the failure against the matching visible draft.
    DraftPersistenceFailed {
        id: UiSessionId,
        draft: SessionDraft,
        reason: String,
    },
    /// A full user turn has crossed its durable acceptance boundary. The
    /// journal entry is projected and the composer is cleared only here, never
    /// by the effect executor that wrote the entry.
    TurnAccepted {
        id: UiSessionId,
        entry: &'a JournalEntry,
        text: String,
        /// `true` means no live runtime can deliver this turn yet, so the
        /// reducer moves the session into Opening while retaining the text.
        queue_runtime: bool,
    },
    /// Durable turn acceptance failed. No transcript or composer mutation is
    /// made; the user keeps their input and sees the reducer-owned reason.
    TurnRejected {
        id: UiSessionId,
        reason: String,
    },
    /// An Agent runtime was created and observed successfully. If a durable
    /// accepted turn is pending, the executor may now deliver that exact text.
    RuntimeAttached {
        id: UiSessionId,
        snapshot: &'a Snapshot,
    },
    /// The runtime acknowledged receipt of the pending accepted turn.
    PendingPostAcknowledged {
        id: UiSessionId,
    },
    /// The effect executor is about to create or recreate a runtime for a
    /// previously accepted turn. This is separate from `TurnAccepted` because
    /// retrying must never project the user message a second time.
    RuntimeStartQueued {
        id: UiSessionId,
        text: String,
    },
    ConnectionStarting,
    ConnectionSucceeded,
    ConnectionFailed {
        reason: String,
        /// Saved turns affected by this failed attempt. Their drafts/messages
        /// stay pending and become explicitly retryable detached sessions.
        affected: Vec<UiSessionId>,
    },
    Notice {
        message: String,
    },
    ComposerCleared {
        id: UiSessionId,
    },
    SessionModelReadiness {
        id: UiSessionId,
        ready: bool,
    },
    SessionNeedsSetup {
        id: UiSessionId,
        message: String,
    },
    /// This process failed to obtain, or intentionally has not yet requested,
    /// the per-session writer. It is a presentation-only overlay: the
    /// durable record remains owned by whichever BONE process holds the lease.
    SessionReadOnlyElsewhere {
        id: UiSessionId,
        message: String,
    },
    /// A product effect acquired a writer for a previously read-only
    /// session and completed a fresh durable hydration/recovery pass.
    SessionWriterAcquired {
        id: UiSessionId,
        record: &'a SessionRecord,
        journal: &'a JournalRead,
        ready_to_attach: bool,
    },
    SessionSelected {
        id: UiSessionId,
    },
    DurableTitleChanged {
        id: UiSessionId,
        title: String,
    },
    FocusSessions,
    /// Hydrate a durable logical session from the SessionStore and journal.
    /// No runtime is required for this projection.
    SessionHydrated {
        id: UiSessionId,
        record: &'a SessionRecord,
        journal: &'a JournalRead,
        show_progress: bool,
        ready_to_attach: bool,
        writer_available: bool,
        select: bool,
    },
    /// The renderer measured the current frame. Recording it through the
    /// reducer keeps terminal layout input on the same one-writer path as
    /// other presentation state.
    ViewportMeasured {
        viewport: Viewport,
    },
}

pub(crate) struct App {
    pub(crate) sessions: Vec<SessionUi>,
    pub(crate) current: usize,
    pub(crate) workspace: String,
    pub(crate) focus: Focus,
    pub(crate) connection: ConnectionState,
    notice: Option<String>,
    viewport: Viewport,
}

impl App {
    pub(crate) fn new(workspace: String) -> Self {
        Self {
            sessions: Vec::new(),
            current: 0,
            workspace,
            focus: Focus::Composer,
            connection: ConnectionState::Disconnected,
            notice: None,
            viewport: Viewport::default(),
        }
    }

    #[cfg(test)]
    pub(crate) fn add_session(
        &mut self,
        id: UiSessionId,
        snapshot: &Snapshot,
        show_progress: bool,
    ) {
        self.sessions.push(SessionUi {
            id,
            durable_id: None,
            durable_title: None,
            conversation: Conversation::new(snapshot, show_progress),
            background_unread: false,
            state: SessionState::Live,
            pending_post: None,
            draft_persistence: DraftPersistenceState::Idle,
        });
        self.current = self.sessions.len() - 1;
        self.focus = Focus::Composer;
    }

    fn add_durable_session_inner(
        &mut self,
        id: UiSessionId,
        record: &SessionRecord,
        journal: &JournalRead,
        show_progress: bool,
        state: SessionState,
        select: bool,
    ) {
        let mut conversation = Conversation::from_journal(journal, show_progress);
        conversation.restore_draft(&record.draft);
        self.sessions.push(SessionUi {
            id,
            durable_id: Some(record.id),
            durable_title: Some(record.metadata.title.clone()),
            conversation,
            background_unread: false,
            state,
            pending_post: None,
            draft_persistence: DraftPersistenceState::Idle,
        });
        if select {
            self.current = self.sessions.len() - 1;
            self.focus = Focus::Composer;
        }
    }

    fn select_session_inner(&mut self, id: UiSessionId) {
        if let Some(index) = self.sessions.iter().position(|session| session.id == id) {
            self.select(index);
            self.focus_composer();
        }
    }

    fn set_session_needs_setup_inner(&mut self, id: UiSessionId, message: String) {
        if let Some(session) = self.sessions.iter_mut().find(|session| session.id == id) {
            session.state = SessionState::NeedsSetup(message);
        }
    }

    fn set_session_read_only_elsewhere_inner(&mut self, id: UiSessionId, message: String) {
        if let Some(session) = self.sessions.iter_mut().find(|session| session.id == id) {
            session.state = SessionState::ReadOnlyElsewhere(message);
        }
    }

    fn refresh_writer_session_inner(
        &mut self,
        id: UiSessionId,
        record: &SessionRecord,
        journal: &JournalRead,
        ready_to_attach: bool,
    ) {
        let Some(session) = self.sessions.iter_mut().find(|session| session.id == id) else {
            return;
        };
        // A read-only session cannot accumulate local composer edits. Rebuild
        // from the fresh journal so a cold-recovery interruption is projected
        // before the user can submit a new turn.
        let mut conversation =
            Conversation::from_journal(journal, session.conversation.projection.show_progress());
        conversation.restore_draft(&record.draft);
        session.conversation = conversation;
        session.durable_title = Some(record.metadata.title.clone());
        session.pending_post = None;
        session.draft_persistence = DraftPersistenceState::Idle;
        session.state = SessionState::from_durable_status(&record.status, ready_to_attach);
    }

    /// Refresh only the model-derived setup state. A background settings
    /// change must not overwrite a live, opening, or offline runtime state.
    fn refresh_session_model_readiness_inner(&mut self, id: UiSessionId, ready: bool) {
        let Some(session) = self.sessions.iter_mut().find(|session| session.id == id) else {
            return;
        };
        if !matches!(
            session.state,
            SessionState::NeedsSetup(_) | SessionState::Detached(_)
        ) {
            return;
        }
        session.state = if ready {
            SessionState::Detached("Ready to start when you send a message".into())
        } else {
            SessionState::NeedsSetup("Choose a model with /model <id> to begin".into())
        };
    }

    pub(crate) fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    pub(crate) fn has_sessions(&self) -> bool {
        !self.sessions.is_empty()
    }

    fn queue_runtime_start_inner(&mut self, id: UiSessionId, text: String) {
        if let Some(session) = self.sessions.iter_mut().find(|session| session.id == id) {
            session.pending_post = Some(text);
            session.state = SessionState::Opening;
        }
    }

    fn apply_journal_entry_inner(&mut self, id: UiSessionId, entry: &JournalEntry) {
        let Some(index) = self.sessions.iter().position(|session| session.id == id) else {
            return;
        };
        let attention = self.sessions[index].conversation.apply_journal_entry(entry);
        if index != self.current || self.focus == Focus::Sessions {
            self.sessions[index].background_unread |= attention;
        }
    }

    fn set_durable_title_inner(&mut self, id: UiSessionId, title: String) {
        if let Some(session) = self.sessions.iter_mut().find(|session| session.id == id) {
            session.durable_title = Some(title);
        }
    }

    fn attach_inner(&mut self, id: UiSessionId, snapshot: &Snapshot) {
        if let Some(session) = self.sessions.iter_mut().find(|session| session.id == id) {
            if session.durable_id.is_some() {
                session.conversation.merge_runtime_snapshot(snapshot);
            } else {
                session.conversation.reset(snapshot);
            }
            session.state = SessionState::Live;
        }
    }

    fn acknowledge_pending_post_inner(&mut self, id: UiSessionId) {
        if let Some(session) = self.sessions.iter_mut().find(|session| session.id == id) {
            session.pending_post = None;
        }
    }

    pub(crate) fn pending_post(&self, id: UiSessionId) -> Option<&str> {
        self.sessions
            .iter()
            .find(|session| session.id == id)
            .and_then(|session| session.pending_post.as_deref())
    }

    #[cfg(test)]
    pub(crate) fn apply(&mut self, id: UiSessionId, step: &StepEvent) {
        let _ = self.reduce(AppEvent::RuntimeStep { id, step });
    }

    fn apply_runtime_step(&mut self, id: UiSessionId, step: &StepEvent) {
        let Some(index) = self.sessions.iter().position(|session| session.id == id) else {
            return;
        };
        let attention = self.sessions[index].conversation.apply(step);
        if index != self.current || self.focus == Focus::Sessions {
            self.sessions[index].background_unread |= attention;
        }
    }

    #[cfg(test)]
    pub(crate) fn reset(&mut self, id: UiSessionId, snapshot: &Snapshot) {
        let _ = self.reduce(AppEvent::RuntimeReset { id, snapshot });
    }

    fn reset_runtime(&mut self, id: UiSessionId, snapshot: &Snapshot) {
        let Some(index) = self.sessions.iter().position(|session| session.id == id) else {
            return;
        };
        let attention = self.sessions[index].conversation.reset(snapshot);
        if index != self.current || self.focus == Focus::Sessions {
            self.sessions[index].background_unread |= attention;
        }
    }

    #[cfg(test)]
    pub(crate) fn mark_offline(&mut self, id: UiSessionId, reason: impl Into<String>) {
        let _ = self.reduce(AppEvent::RuntimeClosed {
            id,
            reason: reason.into(),
        });
    }

    fn mark_runtime_start_failed(&mut self, id: UiSessionId, reason: String) {
        if let Some(session) = self.sessions.iter_mut().find(|session| session.id == id) {
            // `pending_post` intentionally remains intact. It was already
            // journaled and can only be retried by an explicit effect-layer
            // action; the UI must not make it look like ordinary typing.
            session.state = SessionState::Detached(reason);
        }
    }

    fn mark_runtime_closed(&mut self, id: UiSessionId, reason: String) {
        if let Some(session) = self.sessions.iter_mut().find(|session| session.id == id)
            && !matches!(&session.state, SessionState::Offline(_))
        {
            session.state = SessionState::Offline(reason);
        }
    }

    #[cfg(test)]
    pub(crate) fn set_viewport(&mut self, viewport: Viewport) {
        let _ = self.reduce(AppEvent::ViewportMeasured { viewport });
    }

    #[cfg(test)]
    pub(crate) fn on_event(&mut self, event: Event) -> Action {
        self.reduce(AppEvent::Terminal(event))
    }

    pub(crate) fn reduce(&mut self, event: AppEvent<'_>) -> Action {
        match event {
            AppEvent::Terminal(event) => self.on_terminal_event(event),
            AppEvent::RuntimeStep { id, step } => {
                self.apply_runtime_step(id, step);
                Action::None
            }
            AppEvent::RuntimeReset { id, snapshot } => {
                self.reset_runtime(id, snapshot);
                Action::None
            }
            AppEvent::RuntimeClosed { id, reason } => {
                self.mark_runtime_closed(id, reason);
                Action::None
            }
            AppEvent::RuntimeStartFailed { id, reason } => {
                self.mark_runtime_start_failed(id, reason);
                Action::None
            }
            AppEvent::DraftPersisted { id, draft } => {
                self.mark_draft_persisted(id, &draft);
                Action::None
            }
            AppEvent::DraftPersistenceFailed { id, draft, reason } => {
                self.mark_draft_persistence_failed(id, &draft, reason);
                Action::None
            }
            AppEvent::TurnAccepted {
                id,
                entry,
                text,
                queue_runtime,
            } => {
                self.accept_turn(id, entry, text, queue_runtime);
                Action::None
            }
            AppEvent::TurnRejected { id, reason } => {
                self.reject_turn(id, reason);
                Action::None
            }
            AppEvent::RuntimeAttached { id, snapshot } => {
                self.attach_inner(id, snapshot);
                Action::None
            }
            AppEvent::PendingPostAcknowledged { id } => {
                self.acknowledge_pending_post_inner(id);
                Action::None
            }
            AppEvent::RuntimeStartQueued { id, text } => {
                self.queue_runtime_start_inner(id, text);
                Action::None
            }
            AppEvent::ConnectionStarting => {
                self.connection = ConnectionState::Connecting;
                Action::None
            }
            AppEvent::ConnectionSucceeded => {
                self.connection = ConnectionState::Connected;
                Action::None
            }
            AppEvent::ConnectionFailed { reason, affected } => {
                self.connection = ConnectionState::Failed(reason.clone());
                for id in affected {
                    self.mark_runtime_start_failed(id, reason.clone());
                }
                self.notice = Some(format!(
                    "Connection failed; saved messages remain queued: {reason}"
                ));
                Action::None
            }
            AppEvent::Notice { message } => {
                self.notice = Some(message);
                Action::None
            }
            AppEvent::ComposerCleared { id } => {
                self.clear_composer_inner(id);
                Action::None
            }
            AppEvent::SessionModelReadiness { id, ready } => {
                self.refresh_session_model_readiness_inner(id, ready);
                Action::None
            }
            AppEvent::SessionNeedsSetup { id, message } => {
                self.set_session_needs_setup_inner(id, message);
                Action::None
            }
            AppEvent::SessionReadOnlyElsewhere { id, message } => {
                self.set_session_read_only_elsewhere_inner(id, message);
                Action::None
            }
            AppEvent::SessionWriterAcquired {
                id,
                record,
                journal,
                ready_to_attach,
            } => {
                self.refresh_writer_session_inner(id, record, journal, ready_to_attach);
                Action::None
            }
            AppEvent::SessionSelected { id } => {
                self.select_session_inner(id);
                Action::None
            }
            AppEvent::DurableTitleChanged { id, title } => {
                self.set_durable_title_inner(id, title);
                Action::None
            }
            AppEvent::FocusSessions => {
                self.focus = Focus::Sessions;
                Action::None
            }
            AppEvent::SessionHydrated {
                id,
                record,
                journal,
                show_progress,
                ready_to_attach,
                writer_available,
                select,
            } => {
                let state = if writer_available {
                    SessionState::from_durable_status(&record.status, ready_to_attach)
                } else {
                    SessionState::ReadOnlyElsewhere(
                        "Select this conversation to request editing access".into(),
                    )
                };
                self.add_durable_session_inner(id, record, journal, show_progress, state, select);
                Action::None
            }
            AppEvent::ViewportMeasured { viewport } => {
                self.viewport = viewport;
                Action::None
            }
        }
    }

    fn mark_draft_persisted(&mut self, id: UiSessionId, draft: &SessionDraft) {
        let Some(session) = self.sessions.iter_mut().find(|session| session.id == id) else {
            return;
        };
        // A newer terminal edit can arrive before a slower persistence result.
        // Do not let that older result claim the new text has already been
        // saved.
        if session.conversation.draft() == *draft {
            session.draft_persistence = DraftPersistenceState::Saved;
        }
    }

    fn mark_draft_persistence_failed(
        &mut self,
        id: UiSessionId,
        draft: &SessionDraft,
        reason: String,
    ) {
        let Some(session) = self.sessions.iter_mut().find(|session| session.id == id) else {
            return;
        };
        // The result belongs to a superseded draft. The user-facing current
        // text may already have been saved by a later operation, so suppress
        // a stale error rather than implying it applies to their current work.
        if session.conversation.draft() != *draft {
            return;
        }
        session.draft_persistence = DraftPersistenceState::Failed(reason.clone());
        self.notice = Some(format!(
            "Draft is still on screen but was not saved: {reason}"
        ));
    }

    fn accept_turn(
        &mut self,
        id: UiSessionId,
        entry: &JournalEntry,
        text: String,
        queue_runtime: bool,
    ) {
        self.apply_journal_entry_inner(id, entry);
        let Some(session) = self.sessions.iter_mut().find(|session| session.id == id) else {
            return;
        };
        session.conversation.clear_composer();
        session.draft_persistence = DraftPersistenceState::Saved;
        // A receipt from the runtime is a distinct effect boundary. Keep the
        // text here until that receipt arrives so a delivery failure can be
        // retried without reconstructing it from UI text.
        session.pending_post = Some(text);
        if queue_runtime {
            session.state = SessionState::Opening;
        }
    }

    fn reject_turn(&mut self, id: UiSessionId, reason: String) {
        if self.sessions.iter().any(|session| session.id == id) {
            self.notice = Some(reason);
        }
    }

    fn on_terminal_event(&mut self, event: Event) -> Action {
        if let Event::Key(key) = &event
            && key.kind == KeyEventKind::Press
        {
            let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
            match (key.code, ctrl) {
                (KeyCode::Char('c'), true) => return Action::Quit,
                (KeyCode::Char('n'), true) => return Action::NewSession,
                (KeyCode::Left, true) if self.focus == Focus::Composer => {
                    self.focus = Focus::Sessions;
                    return Action::None;
                }
                (KeyCode::Right, true) if self.focus == Focus::Sessions => {
                    self.focus_composer();
                    return Action::None;
                }
                _ => {}
            }
        }

        if self.focus == Focus::Sessions {
            if let Event::Key(key) = &event
                && key.kind == KeyEventKind::Press
                && key.modifiers == KeyModifiers::NONE
            {
                match key.code {
                    KeyCode::Up => {
                        self.select_previous();
                        return Action::None;
                    }
                    KeyCode::Down => {
                        self.select_next();
                        return Action::None;
                    }
                    KeyCode::Enter | KeyCode::Esc | KeyCode::Right => {
                        self.focus_composer();
                        return Action::None;
                    }
                    _ => {}
                }
            }

            let resumes_composing = matches!(&event, Event::Paste(_))
                || matches!(
                    &event,
                    Event::Key(key)
                        if key.kind != KeyEventKind::Release
                            && matches!(
                                key.code,
                                KeyCode::Char(_) | KeyCode::Backspace | KeyCode::Delete
                            )
                );
            if !resumes_composing {
                return Action::None;
            }
            self.focus_composer();
        }

        let session = &mut self.sessions[self.current];
        if let SessionState::ReadOnlyElsewhere(message) = &session.state {
            // Scrolling remains useful for a read-only transcript, but no
            // composer edit, command, send, or stop action may escape the
            // reducer. This prevents an unsaved ghost draft if the effect
            // executor cannot obtain the writer.
            let navigation = matches!(
                &event,
                Event::Key(key)
                    if key.kind != KeyEventKind::Release
                        && matches!(
                            key.code,
                            KeyCode::PageUp | KeyCode::PageDown | KeyCode::Home | KeyCode::End
                        )
            );
            if !navigation {
                self.notice = Some(message.clone());
                return Action::None;
            }
        }
        let before_draft = session.conversation.draft();
        match session.conversation.on_event(event, self.viewport) {
            ConversationAction::None if session.conversation.draft() != before_draft => {
                session.draft_persistence = DraftPersistenceState::Pending;
                Action::DraftChanged {
                    id: session.id,
                    draft: session.conversation.draft(),
                }
            }
            ConversationAction::None => Action::None,
            ConversationAction::Post(text) if session.state == SessionState::Live => Action::Post {
                id: session.id,
                text,
            },
            ConversationAction::Post(text)
                if session.state == SessionState::Opening && session.pending_post.is_none() =>
            {
                session.pending_post = Some(text);
                session.conversation.clear_composer();
                Action::None
            }
            ConversationAction::Post(text)
                if matches!(session.state, SessionState::Detached(_)) =>
            {
                Action::Post {
                    id: session.id,
                    text,
                }
            }
            ConversationAction::Post(_) if matches!(session.state, SessionState::NeedsSetup(_)) => {
                let message = match &session.state {
                    SessionState::NeedsSetup(message) => message.clone(),
                    _ => unreachable!("matched needs-setup session state"),
                };
                self.notice = Some(message);
                Action::None
            }
            ConversationAction::Stop { clear } if session.state == SessionState::Live => {
                Action::Stop {
                    id: session.id,
                    clear,
                }
            }
            ConversationAction::Command(command) => Action::Command {
                id: session.id,
                command,
            },
            ConversationAction::Feedback(message) => {
                self.notice = Some(message);
                Action::None
            }
            ConversationAction::Post(_) | ConversationAction::Stop { .. } => {
                if session.conversation.draft() != before_draft {
                    session.draft_persistence = DraftPersistenceState::Pending;
                    Action::DraftChanged {
                        id: session.id,
                        draft: session.conversation.draft(),
                    }
                } else {
                    Action::None
                }
            }
            ConversationAction::Quit => Action::Quit,
        }
    }

    fn clear_composer_inner(&mut self, id: UiSessionId) {
        if let Some(session) = self.sessions.iter_mut().find(|session| session.id == id) {
            session.conversation.clear_composer();
            session.draft_persistence = DraftPersistenceState::Idle;
        }
    }

    pub(crate) fn current(&self) -> &SessionUi {
        &self.sessions[self.current]
    }

    /// The product runner uses this after selection changes to lazily acquire
    /// the current logical session's writer before accepting any edit or
    /// runtime effect.
    pub(crate) fn current_id(&self) -> UiSessionId {
        self.current().id
    }

    fn select_previous(&mut self) {
        if self.sessions.len() > 1 {
            self.select((self.current + self.sessions.len() - 1) % self.sessions.len());
        }
    }

    fn select_next(&mut self) {
        if self.sessions.len() > 1 {
            self.select((self.current + 1) % self.sessions.len());
        }
    }

    fn select(&mut self, index: usize) {
        self.current = index;
    }

    fn focus_composer(&mut self) {
        self.focus = Focus::Composer;
        self.sessions[self.current].background_unread = false;
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Focus {
    Sessions,
    #[default]
    Composer,
}

pub(crate) struct SessionUi {
    pub(crate) id: UiSessionId,
    /// Persistent logical identity for product sessions. `None` is reserved
    /// for reducer and renderer test fixtures.
    pub(crate) durable_id: Option<crate::SessionId>,
    durable_title: Option<String>,
    pub(crate) conversation: Conversation,
    pub(crate) background_unread: bool,
    pub(crate) state: SessionState,
    pub(crate) pending_post: Option<String>,
    pub(crate) draft_persistence: DraftPersistenceState,
}

impl SessionUi {
    pub(crate) fn title(&self) -> &str {
        if let Some(title) = self
            .durable_title
            .as_deref()
            .filter(|title| *title != "New conversation")
        {
            return title;
        }
        self.conversation
            .projection
            .title()
            .or_else(|| {
                self.pending_post
                    .as_deref()
                    .and_then(|text| text.lines().find(|line| !line.trim().is_empty()))
            })
            .or_else(|| {
                self.conversation
                    .composer
                    .lines()
                    .iter()
                    .find(|line| !line.trim().is_empty())
                    .map(String::as_str)
            })
            .unwrap_or("New conversation")
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SessionState {
    /// The durable session exists, but it still needs an initial model choice.
    NeedsSetup(String),
    /// The durable session can be attached on the next user message. A former
    /// working runtime is deliberately displayed as detached after restart.
    Detached(String),
    /// Another BONE process owns this logical session's writer, or this
    /// process has not intentionally activated it yet. This is never written
    /// to `SessionRecord`: it is a local presentation safeguard only.
    ReadOnlyElsewhere(String),
    Opening,
    Live,
    Offline(String),
}

impl SessionState {
    fn from_durable_status(status: &DurableSessionStatus, ready_to_attach: bool) -> Self {
        if !ready_to_attach {
            return Self::NeedsSetup("Choose a model with /model <id> to begin".into());
        }
        match status.execution {
            crate::SessionExecution::Working
            | crate::SessionExecution::Opening
            | crate::SessionExecution::Stopping
            | crate::SessionExecution::QueuedForRuntime
            | crate::SessionExecution::QueuedForSetup => {
                Self::Detached("Previous work was interrupted; ready for your next message".into())
            }
            crate::SessionExecution::Offline => {
                Self::Detached("Offline previously; ready to retry on your next message".into())
            }
            _ => Self::Detached("Ready to start when you send a message".into()),
        }
    }
}

pub(crate) struct Conversation {
    pub(super) projection: Projection,
    pub(crate) composer: TextArea<'static>,
    pub(crate) anchor: Option<ScrollAnchor>,
    pub(crate) unread: bool,
    provenance: InputProvenance,
    runtime_overlay: Option<RuntimeOverlay>,
    cursor: u64,
}

/// Runtime record cursors are process-local and restart at one; durable
/// journal sequences do not. Keep a separate overlay cursor so a newly
/// attached runtime can add live events to a restored history without
/// replacing it or reusing presentation cursors.
#[derive(Clone, Copy, Debug)]
struct RuntimeOverlay {
    presentation_offset: u64,
    record_cursor: u64,
}

impl Conversation {
    fn empty(show_progress: bool) -> Self {
        Self {
            projection: Projection::empty(show_progress),
            composer: composer(),
            anchor: None,
            unread: false,
            provenance: InputProvenance::TypedOnly,
            runtime_overlay: None,
            cursor: 0,
        }
    }

    #[cfg(test)]
    fn new(snapshot: &Snapshot, show_progress: bool) -> Self {
        let mut conversation = Self::empty(show_progress);
        conversation.projection = Projection::from_snapshot(snapshot, show_progress);
        conversation.cursor = snapshot.record_cursor;
        conversation
    }

    fn from_journal(journal: &JournalRead, show_progress: bool) -> Self {
        let mut conversation = Self::empty(show_progress);
        for entry in &journal.entries {
            conversation.apply_journal_entry(entry);
        }
        conversation
    }

    fn apply_journal_entry(&mut self, entry: &JournalEntry) -> bool {
        self.cursor = self.cursor.max(entry.sequence.value());
        match &entry.fact {
            JournalFact::UserTurnAccepted { text, .. } => {
                self.projection.timeline.push(TimelineItem::message(
                    entry.sequence.value(),
                    Speaker::User,
                    text.clone(),
                    false,
                ));
                self.projection.status = SessionStatus::Working;
                false
            }
            JournalFact::AssistantReply { text } => {
                self.projection.timeline.push(TimelineItem::message(
                    entry.sequence.value(),
                    Speaker::Bone,
                    text.clone(),
                    true,
                ));
                true
            }
            JournalFact::RuntimeInterrupted { reason } => {
                self.projection.timeline.push(TimelineItem::status(
                    entry.sequence.value(),
                    format!("— interrupted: {reason}"),
                    Tone::Warning,
                    true,
                ));
                self.projection.status = SessionStatus::Stopped;
                true
            }
            JournalFact::UnresolvedExternalEffect { summary } => {
                self.projection.timeline.push(TimelineItem::status(
                    entry.sequence.value(),
                    format!("— action may need review: {summary}"),
                    Tone::Error,
                    true,
                ));
                true
            }
            JournalFact::TurnFinished { outcome, .. } => {
                self.projection.status = match outcome {
                    crate::TurnOutcome::Completed => SessionStatus::Complete,
                    crate::TurnOutcome::WaitingForUser => SessionStatus::Waiting,
                    crate::TurnOutcome::Stopped | crate::TurnOutcome::Failed => {
                        SessionStatus::Stopped
                    }
                };
                false
            }
            JournalFact::TurnStarted { .. } => {
                self.projection.status = SessionStatus::Working;
                false
            }
        }
    }

    fn apply(&mut self, step: &StepEvent) -> bool {
        let before = self.projection.timeline.len();
        if self.runtime_overlay.is_some() {
            for entry in &step.records {
                self.apply_runtime_record(entry);
            }
        } else {
            self.projection.apply_all(&step.records);
            if let Some(entry) = step.records.last() {
                self.cursor = entry.cursor;
            }
        }
        let added = &self.projection.timeline[before..];
        if self.anchor.is_some() && !added.is_empty() {
            self.unread = true;
        }
        added.iter().any(|item| item.attention) || step.records.iter().any(is_finished_notice)
    }

    fn reset(&mut self, snapshot: &Snapshot) -> bool {
        if self.runtime_overlay.is_some() {
            let before = self.projection.timeline.len();
            let previous_runtime_cursor = self.runtime_record_cursor();
            for entry in &snapshot.record {
                self.apply_runtime_record(entry);
            }
            let added = &self.projection.timeline[before..];
            if self.anchor.is_some() && !added.is_empty() {
                self.unread = true;
            }
            return added.iter().any(|item| item.attention)
                || snapshot.record.iter().any(|entry| {
                    entry.cursor > previous_runtime_cursor && is_finished_notice(entry)
                });
        }
        let latest = self.cursor;
        let show_progress = self.projection.show_progress();
        self.projection = Projection::from_snapshot(snapshot, show_progress);
        self.cursor = snapshot.record_cursor;
        let first = self
            .projection
            .timeline
            .partition_point(|item| item.cursor <= latest);
        let new_items = &self.projection.timeline[first..];
        let attention = new_items.iter().any(|item| item.attention)
            || snapshot
                .record
                .iter()
                .any(|entry| entry.cursor > latest && is_finished_notice(entry));

        if let Some(anchor) = self.anchor {
            self.anchor = self
                .projection
                .timeline
                .iter()
                .find(|item| item.cursor >= anchor.cursor)
                .map(|item| ScrollAnchor {
                    cursor: item.cursor,
                    line: anchor.line,
                });
            self.unread |= !new_items.is_empty();
        } else {
            self.unread = false;
        }
        attention
    }

    fn merge_runtime_snapshot(&mut self, snapshot: &Snapshot) -> bool {
        self.runtime_overlay = Some(RuntimeOverlay {
            presentation_offset: self.cursor,
            record_cursor: 0,
        });
        let before = self.projection.timeline.len();
        for entry in &snapshot.record {
            self.apply_runtime_record(entry);
        }
        let added = &self.projection.timeline[before..];
        if self.anchor.is_some() && !added.is_empty() {
            self.unread = true;
        }
        added.iter().any(|item| item.attention) || snapshot.record.iter().any(is_finished_notice)
    }

    fn apply_runtime_record(&mut self, entry: &RecordEntry) {
        let Some(mut overlay) = self.runtime_overlay else {
            self.projection.apply(entry);
            self.cursor = entry.cursor;
            return;
        };
        if entry.cursor <= overlay.record_cursor {
            return;
        }
        overlay.record_cursor = entry.cursor;
        self.runtime_overlay = Some(overlay);
        self.cursor = overlay.presentation_offset.saturating_add(entry.cursor);
        if self.is_duplicate_journal_user(entry) {
            self.projection.status = SessionStatus::Working;
            return;
        }
        let mut presentation_entry = entry.clone();
        presentation_entry.cursor = self.cursor;
        self.projection.apply(&presentation_entry);
    }

    fn runtime_record_cursor(&self) -> u64 {
        self.runtime_overlay
            .map_or(0, |overlay| overlay.record_cursor)
    }

    fn is_duplicate_journal_user(&self, entry: &RecordEntry) -> bool {
        let RecordKind::UserMessage(message) = &entry.kind else {
            return false;
        };
        self.projection
            .timeline
            .iter()
            .rev()
            .find_map(|item| match &item.kind {
                TimelineKind::Message {
                    speaker: Speaker::User,
                    text,
                } => Some(text == &message.text),
                _ => None,
            })
            == Some(true)
    }

    fn on_event(&mut self, event: Event, viewport: Viewport) -> ConversationAction {
        match event {
            Event::Key(key) => self.on_key(key, viewport),
            Event::Paste(text) => {
                self.provenance = InputProvenance::ContainsPaste;
                self.composer
                    .insert_str(text.replace("\r\n", "\n").replace('\r', "\n"));
                ConversationAction::None
            }
            _ => ConversationAction::None,
        }
    }

    fn clear_composer(&mut self) {
        self.composer = composer();
        self.provenance = InputProvenance::TypedOnly;
    }

    fn restore_draft(&mut self, draft: &SessionDraft) {
        if draft.text.is_empty() {
            return;
        }
        self.composer.insert_str(&draft.text);
        let (row, column) = cursor_for_byte_offset(&draft.text, draft.cursor_byte_offset);
        self.composer.move_cursor(CursorMove::Jump(
            row.min(u16::MAX as usize) as u16,
            column.min(u16::MAX as usize) as u16,
        ));
    }

    fn draft(&self) -> SessionDraft {
        let text = self.composer.lines().join("\n");
        let ratatui_textarea::DataCursor(row, column) = self.composer.cursor();
        let offset = byte_offset_for_cursor(self.composer.lines(), row, column);
        SessionDraft::new(text, offset).expect("textarea always has a valid UTF-8 cursor")
    }

    fn on_key(&mut self, key: KeyEvent, viewport: Viewport) -> ConversationAction {
        if key.kind == KeyEventKind::Release {
            return ConversationAction::None;
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if key.kind == KeyEventKind::Press {
            match (key.code, ctrl) {
                (KeyCode::Char('j'), true) => {
                    self.composer.insert_newline();
                    return ConversationAction::None;
                }
                (KeyCode::Esc, _) => return ConversationAction::Stop { clear: false },
                (KeyCode::PageUp, _) => {
                    self.scroll_up(viewport);
                    return ConversationAction::None;
                }
                (KeyCode::PageDown, _) => {
                    self.scroll_down(viewport);
                    return ConversationAction::None;
                }
                (KeyCode::Home, true) => {
                    self.scroll_to_top();
                    return ConversationAction::None;
                }
                (KeyCode::End, true) => {
                    self.scroll_to_end();
                    return ConversationAction::None;
                }
                (KeyCode::Enter, false) => return self.submit(),
                _ => {}
            }
        } else if key.code == KeyCode::Enter {
            return ConversationAction::None;
        }

        self.composer.input(key);
        ConversationAction::None
    }

    fn submit(&self) -> ConversationAction {
        let text = self.composer.lines().join("\n");
        match parse_submission(text, self.provenance) {
            Submission::Empty => ConversationAction::None,
            Submission::Message(text) | Submission::EscapedMessage(text) => {
                ConversationAction::Post(text)
            }
            Submission::Command(LocalCommand::Stop) => ConversationAction::Stop { clear: true },
            Submission::Command(LocalCommand::Exit) => ConversationAction::Quit,
            Submission::Command(command) => ConversationAction::Command(command),
            Submission::Unknown {
                name, suggestions, ..
            } => ConversationAction::Feedback(command_suggestion(&name, &suggestions)),
            Submission::Invalid { message, .. } => ConversationAction::Feedback(message.into()),
        }
    }

    fn scroll_up(&mut self, viewport: Viewport) {
        let Some(layout) = TimelineLayout::new(&self.projection.timeline, viewport) else {
            return;
        };
        let current = layout.top(self.anchor);
        if current == 0 {
            return;
        }
        self.anchor = Some(layout.anchor(current.saturating_sub(viewport.page())));
    }

    fn scroll_down(&mut self, viewport: Viewport) {
        let Some(anchor) = self.anchor else {
            return;
        };
        let Some(layout) = TimelineLayout::new(&self.projection.timeline, viewport) else {
            return;
        };
        let next = layout.top(Some(anchor)).saturating_add(viewport.page());
        if next >= layout.tail {
            self.scroll_to_end();
        } else {
            self.anchor = Some(layout.anchor(next));
        }
    }

    fn scroll_to_top(&mut self) {
        self.anchor = self.projection.timeline.first().map(|item| ScrollAnchor {
            cursor: item.cursor,
            line: 0,
        });
    }

    fn scroll_to_end(&mut self) {
        self.anchor = None;
        self.unread = false;
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Viewport {
    pub(crate) width: u16,
    pub(crate) height: u16,
    pub(crate) spacing: usize,
}

impl Viewport {
    fn page(self) -> usize {
        usize::from(self.height).saturating_sub(1).max(1)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ScrollAnchor {
    pub(crate) cursor: u64,
    pub(crate) line: usize,
}

struct TimelineLayout<'a> {
    rows: Vec<(&'a TimelineItem, usize, usize)>,
    tail: usize,
}

impl<'a> TimelineLayout<'a> {
    fn new(items: &'a [TimelineItem], viewport: Viewport) -> Option<Self> {
        if items.is_empty() || viewport.width == 0 || viewport.height == 0 {
            return None;
        }
        let mut top = 0;
        let rows = items
            .iter()
            .map(|item| {
                let height = item.line_count(viewport.width);
                let row = (item, top, height);
                top += height + viewport.spacing;
                row
            })
            .collect::<Vec<_>>();
        top = top.saturating_sub(viewport.spacing);
        Some(Self {
            rows,
            tail: top.saturating_sub(usize::from(viewport.height)),
        })
    }

    fn top(&self, anchor: Option<ScrollAnchor>) -> usize {
        let Some(anchor) = anchor else {
            return self.tail;
        };
        self.rows
            .iter()
            .find(|(item, _, _)| item.cursor == anchor.cursor)
            .map_or(self.tail, |(_, top, height)| {
                top + anchor.line.min(height.saturating_sub(1))
            })
    }

    fn anchor(&self, target: usize) -> ScrollAnchor {
        for (item, top, height) in &self.rows {
            if target < top + height {
                return ScrollAnchor {
                    cursor: item.cursor,
                    line: target.saturating_sub(*top),
                };
            }
        }
        let (item, _, height) = self.rows.last().expect("a timeline row exists");
        ScrollAnchor {
            cursor: item.cursor,
            line: height.saturating_sub(1),
        }
    }
}

fn composer() -> TextArea<'static> {
    let mut composer = TextArea::default();
    composer.set_wrap_mode(WrapMode::WordOrGlyph);
    composer.set_placeholder_text("Ask BONE…");
    composer.set_placeholder_style(Style::default().dim());
    composer.set_cursor_line_style(Style::default());
    composer
}

fn byte_offset_for_cursor(lines: &[String], row: usize, column: usize) -> usize {
    let prior = lines
        .iter()
        .take(row.min(lines.len()))
        .map(|line| line.len() + 1)
        .sum::<usize>();
    let Some(line) = lines.get(row) else {
        return prior;
    };
    let column_bytes = line
        .char_indices()
        .nth(column)
        .map_or(line.len(), |(offset, _)| offset);
    prior + column_bytes
}

fn cursor_for_byte_offset(text: &str, byte_offset: usize) -> (usize, usize) {
    let mut remaining = byte_offset.min(text.len());
    let mut row = 0;
    for line in text.split('\n') {
        if remaining <= line.len() {
            let column = line[..remaining].chars().count();
            return (row, column);
        }
        remaining = remaining.saturating_sub(line.len() + 1);
        row += 1;
    }
    let line = text.rsplit('\n').next().unwrap_or_default();
    (row.saturating_sub(1), line.chars().count())
}

fn command_suggestion(name: &str, suggestions: &[&super::commands::CommandDescriptor]) -> String {
    if suggestions.is_empty() {
        return format!("Unknown command /{name}. Use //{name} to send it as a message.");
    }
    let choices = suggestions
        .iter()
        .map(|descriptor| format!("/{}", descriptor.name))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "Unknown command /{name}. Did you mean {choices}? Use //{name} to send it as a message."
    )
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Action {
    None,
    DraftChanged {
        id: UiSessionId,
        draft: SessionDraft,
    },
    Post {
        id: UiSessionId,
        text: String,
    },
    Stop {
        id: UiSessionId,
        clear: bool,
    },
    Command {
        id: UiSessionId,
        command: LocalCommand,
    },
    NewSession,
    Quit,
}

#[derive(Debug, PartialEq, Eq)]
enum ConversationAction {
    None,
    Post(String),
    Stop { clear: bool },
    Command(LocalCommand),
    Feedback(String),
    Quit,
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::{JournalEntry, JournalFact, JournalSequence, UnixMillis};
    use bone_agent::{
        EffectSummary, Event as AgentEvent, JobOutcome, Message, MessageId, Notice, RecordEntry,
        RecordKind, Snapshot, StepEvent, ToolCall,
    };
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    use super::super::agent_projection::{Projection, SessionStatus, Speaker, TimelineKind};
    use super::{
        Action, App, AppEvent, ConnectionState, DraftPersistenceState, Focus, ScrollAnchor,
        SessionState, UiSessionId, Viewport,
    };

    #[test]
    fn composer_distinguishes_send_newline_repeat_and_stop() {
        let mut app = App::new("workspace".into());
        app.add_session(UiSessionId(1), &snapshot(vec![]), true);
        app.on_event(Event::Paste("你好\r\n🙂e\u{301}\rline".into()));
        assert_eq!(
            app.current().conversation.composer.lines(),
            ["你好", "🙂e\u{301}", "line"]
        );

        assert_eq!(
            app.on_event(Event::Key(key(
                KeyCode::Enter,
                KeyModifiers::NONE,
                KeyEventKind::Repeat,
            ))),
            Action::None
        );
        app.on_event(Event::Key(key(
            KeyCode::Char('j'),
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        )));
        assert_eq!(app.current().conversation.composer.lines().len(), 4);
        assert!(matches!(
            app.on_event(Event::Key(key(
                KeyCode::Enter,
                KeyModifiers::NONE,
                KeyEventKind::Press,
            ))),
            Action::Post { id: UiSessionId(1), text }
                if text == "你好\n🙂e\u{301}\nline\n"
        ));

        let draft = app.current().conversation.composer.lines().to_vec();
        assert_eq!(
            app.on_event(Event::Key(key(
                KeyCode::Esc,
                KeyModifiers::NONE,
                KeyEventKind::Press,
            ))),
            Action::Stop {
                id: UiSessionId(1),
                clear: false,
            }
        );
        assert_eq!(app.current().conversation.composer.lines(), draft);
    }

    #[test]
    fn new_conversation_always_means_new_conversation() {
        let mut app = App::new("workspace".into());
        app.add_session(UiSessionId(1), &snapshot(vec![]), true);

        assert_eq!(
            app.on_event(Event::Key(key(
                KeyCode::Char('n'),
                KeyModifiers::CONTROL,
                KeyEventKind::Press,
            ))),
            Action::NewSession
        );
        assert_eq!(app.current().id, UiSessionId(1));
    }

    #[test]
    fn read_only_elsewhere_blocks_drafts_posts_and_local_commands_in_the_reducer() {
        let mut app = App::new("workspace".into());
        app.add_session(UiSessionId(1), &snapshot(vec![]), true);
        let _ = app.reduce(AppEvent::SessionReadOnlyElsewhere {
            id: UiSessionId(1),
            message: "Open in another BONE process".into(),
        });

        assert_eq!(
            app.reduce(AppEvent::Terminal(Event::Key(key(
                KeyCode::Char('x'),
                KeyModifiers::NONE,
                KeyEventKind::Press,
            )))),
            Action::None
        );
        assert_eq!(app.current().conversation.composer.lines(), [""]);

        // This simulates an already-visible slash command at the exact
        // reducer boundary. It must not escape as Action::Command or clear
        // the text just because the session became read-only concurrently.
        app.sessions[app.current]
            .conversation
            .composer
            .insert_str("/status");
        assert_eq!(
            app.reduce(AppEvent::Terminal(Event::Key(key(
                KeyCode::Enter,
                KeyModifiers::NONE,
                KeyEventKind::Press,
            )))),
            Action::None
        );
        assert_eq!(app.current().conversation.composer.lines(), ["/status"]);
        assert_eq!(app.notice(), Some("Open in another BONE process"));
        assert!(matches!(
            app.current().state,
            SessionState::ReadOnlyElsewhere(_)
        ));
    }

    #[test]
    fn incremental_projection_matches_snapshot_rebuild() {
        let records = conversation_records();
        let mut incremental = Projection::from_snapshot(&snapshot(vec![]), true);
        incremental.apply_all(&records[..3]);
        incremental.apply_all(&records[3..]);
        let rebuilt = Projection::from_snapshot(&snapshot(records), true);
        assert_eq!(incremental, rebuilt);
    }

    #[test]
    fn reset_keeps_the_draft_anchor_and_counts_missed_items() {
        let initial = vec![user(1, "one"), reply(2, "two")];
        let mut app = App::new("workspace".into());
        app.add_session(UiSessionId(1), &snapshot(initial.clone()), true);
        app.on_event(Event::Paste("unfinished 草稿".into()));
        app.sessions[0].conversation.anchor = Some(ScrollAnchor { cursor: 1, line: 0 });
        app.sessions[0].conversation.unread = true;

        let mut recovered = initial;
        recovered.push(user(3, "three"));
        recovered.push(reply(4, "four"));
        app.reset(UiSessionId(1), &snapshot(recovered));

        let conversation = &app.current().conversation;
        assert_eq!(conversation.composer.lines(), ["unfinished 草稿"]);
        assert_eq!(
            conversation.anchor,
            Some(ScrollAnchor { cursor: 1, line: 0 })
        );
        assert!(conversation.unread);
    }

    #[test]
    fn background_updates_and_session_switching_keep_each_draft_independent() {
        let mut app = App::new("workspace".into());
        app.add_session(UiSessionId(1), &snapshot(vec![user(1, "first")]), true);
        app.on_event(Event::Paste("draft one".into()));
        app.add_session(UiSessionId(2), &snapshot(vec![]), true);
        app.on_event(Event::Paste("draft two".into()));

        app.apply(
            UiSessionId(1),
            &step(vec![reply(2, "finished in background")]),
        );
        assert!(app.sessions[0].background_unread);
        assert_eq!(app.sessions[1].conversation.composer.lines(), ["draft two"]);

        app.on_event(Event::Key(key(
            KeyCode::Left,
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        )));
        app.on_event(Event::Key(key(
            KeyCode::Up,
            KeyModifiers::NONE,
            KeyEventKind::Press,
        )));
        assert_eq!(app.current().id, UiSessionId(1));
        assert!(app.current().background_unread);
        app.on_event(Event::Key(key(
            KeyCode::Right,
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        )));
        assert_eq!(app.current().id, UiSessionId(1));
        assert!(!app.current().background_unread);
        assert_eq!(app.current().conversation.composer.lines(), ["draft one"]);
    }

    #[test]
    fn session_focus_keeps_new_activity_unread_until_returning_to_composer() {
        let mut app = App::new("workspace".into());
        app.add_session(UiSessionId(1), &snapshot(vec![user(1, "first")]), true);

        app.on_event(Event::Key(key(
            KeyCode::Left,
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        )));
        app.apply(UiSessionId(1), &step(vec![reply(2, "arrived in the list")]));
        assert!(app.current().background_unread);

        app.on_event(Event::Key(key(
            KeyCode::Right,
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        )));
        assert!(!app.current().background_unread);
    }

    #[test]
    fn background_finish_is_unread_without_becoming_a_timeline_row() {
        let mut app = App::new("workspace".into());
        app.add_session(UiSessionId(1), &snapshot(vec![user(1, "first")]), true);
        app.add_session(UiSessionId(2), &snapshot(vec![user(2, "second")]), true);
        let timeline_len = app.sessions[0].conversation.projection.timeline.len();

        app.apply(
            UiSessionId(1),
            &step(vec![RecordEntry {
                cursor: 2,
                kind: RecordKind::Notice(Notice::Finished { cleanup: vec![] }),
            }]),
        );

        assert!(app.sessions[0].background_unread);
        assert_eq!(
            app.sessions[0].conversation.projection.status,
            SessionStatus::Complete
        );
        assert_eq!(
            app.sessions[0].conversation.projection.timeline.len(),
            timeline_len
        );
    }

    #[test]
    fn quiet_mode_hides_success_but_keeps_failed_and_unknown_tools() {
        let records = vec![
            tool_started(1, 1, "read"),
            tool_finished(2, 1, JobOutcome::artifact("ok")),
            tool_started(3, 2, "grep"),
            tool_finished(4, 2, JobOutcome::failed("pattern rejected")),
            tool_started(5, 3, "write"),
            tool_finished(6, 3, JobOutcome::unknown("connection lost")),
            tool_finished(7, 3, JobOutcome::artifact("confirmed")),
        ];
        let projection = Projection::from_snapshot(&snapshot(records), false);
        assert_eq!(projection.timeline.len(), 3);
        assert!(matches!(
            &projection.timeline[0].kind,
            TimelineKind::Tool { text, .. } if text == "× grep · pattern rejected"
        ));
        assert!(matches!(
            &projection.timeline[1].kind,
            TimelineKind::Tool { text, .. }
                if text == "! write · outcome unknown: connection lost"
        ));
        assert!(matches!(
            &projection.timeline[2].kind,
            TimelineKind::Tool { text, .. } if text == "✓ write · outcome resolved"
        ));
        assert!(!projection.has_unknown_effect());
    }

    #[test]
    fn ordinary_tool_cancellation_does_not_look_like_a_failed_result() {
        let cancelled = JobOutcome {
            result: Err(bone_agent::JobError {
                kind: bone_agent::JobErrorKind::Cancelled,
                message: "cancelled during cleanup".into(),
            }),
            external_effect: bone_agent::ExternalEffect::None,
        };
        let projection = Projection::from_snapshot(
            &snapshot(vec![
                tool_started(1, 1, "read"),
                tool_finished(2, 1, cancelled),
            ]),
            true,
        );

        assert!(projection.timeline.is_empty());
        assert!(!projection.has_unknown_effect());
    }

    #[test]
    fn cancelled_write_with_an_unknown_outcome_stays_visible() {
        let cancelled = JobOutcome {
            result: Err(bone_agent::JobError {
                kind: bone_agent::JobErrorKind::Cancelled,
                message: "cancel refused after commit started".into(),
            }),
            external_effect: bone_agent::ExternalEffect::Unknown,
        };
        let projection = Projection::from_snapshot(
            &snapshot(vec![
                tool_started(1, 1, "write"),
                tool_finished(2, 1, cancelled),
            ]),
            false,
        );

        assert!(projection.has_unknown_effect());
        assert!(matches!(
            &projection.timeline[0].kind,
            TimelineKind::Tool { text, .. }
                if text == "! write · outcome unknown: cancel refused after commit started"
        ));
    }

    #[test]
    fn runtime_attach_preserves_a_background_conversation_draft_and_focus() {
        let mut app = App::new("workspace".into());
        app.add_session(UiSessionId(1), &snapshot(vec![user(1, "first")]), true);

        assert_eq!(
            app.on_event(Event::Key(key(
                KeyCode::Char('n'),
                KeyModifiers::CONTROL,
                KeyEventKind::Press,
            ))),
            Action::NewSession
        );
        app.add_session(UiSessionId(2), &snapshot(vec![]), true);
        let entry = accepted_turn_entry(1, "saved message for two");
        let _ = app.reduce(AppEvent::TurnAccepted {
            id: UiSessionId(2),
            entry: &entry,
            text: "saved message for two".into(),
            queue_runtime: true,
        });
        assert!(matches!(
            app.on_event(Event::Paste("draft for two".into())),
            Action::DraftChanged {
                id: UiSessionId(2),
                ..
            }
        ));

        app.on_event(Event::Key(key(
            KeyCode::Left,
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        )));
        app.on_event(Event::Key(key(
            KeyCode::Up,
            KeyModifiers::NONE,
            KeyEventKind::Press,
        )));
        app.on_event(Event::Key(key(
            KeyCode::Right,
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        )));
        let _ = app.reduce(AppEvent::RuntimeAttached {
            id: UiSessionId(2),
            snapshot: &snapshot(vec![]),
        });

        assert_eq!(app.current().id, UiSessionId(1));
        assert_eq!(app.sessions[1].state, SessionState::Live);
        assert_eq!(
            app.pending_post(UiSessionId(2)),
            Some("saved message for two")
        );
        assert_eq!(
            app.sessions[1].conversation.composer.lines(),
            ["draft for two"]
        );

        let _ = app.reduce(AppEvent::RuntimeClosed {
            id: UiSessionId(2),
            reason: "connection closed".into(),
        });
        assert_eq!(app.sessions[0].state, SessionState::Live);
        assert_eq!(
            app.sessions[1].state,
            SessionState::Offline("connection closed".into())
        );
    }

    #[test]
    fn durable_pending_post_is_kept_until_the_runtime_receipt() {
        let mut app = App::new("workspace".into());
        app.add_session(UiSessionId(1), &snapshot(vec![]), true);
        let entry = accepted_turn_entry(1, "send after opening");
        let _ = app.reduce(AppEvent::TurnAccepted {
            id: UiSessionId(1),
            entry: &entry,
            text: "send after opening".into(),
            queue_runtime: true,
        });
        let _ = app.reduce(AppEvent::RuntimeAttached {
            id: UiSessionId(1),
            snapshot: &snapshot(vec![]),
        });

        assert_eq!(
            app.sessions[0].pending_post.as_deref(),
            Some("send after opening")
        );

        let _ = app.reduce(AppEvent::PendingPostAcknowledged { id: UiSessionId(1) });
        assert_eq!(app.sessions[0].pending_post, None);
    }

    #[test]
    fn runtime_start_failure_leaves_the_saved_pending_post_retryable() {
        let mut app = App::new("workspace".into());
        app.add_session(UiSessionId(1), &snapshot(vec![]), true);
        let entry = accepted_turn_entry(1, "durably accepted task");
        let _ = app.reduce(AppEvent::TurnAccepted {
            id: UiSessionId(1),
            entry: &entry,
            text: "durably accepted task".into(),
            queue_runtime: true,
        });

        let _ = app.reduce(AppEvent::RuntimeStartFailed {
            id: UiSessionId(1),
            reason: "connection unavailable; use /login to retry".into(),
        });

        assert_eq!(
            app.sessions[0].state,
            SessionState::Detached("connection unavailable; use /login to retry".into())
        );
        assert_eq!(
            app.sessions[0].pending_post.as_deref(),
            Some("durably accepted task")
        );
    }

    #[test]
    fn failed_runtime_delivery_keeps_the_saved_post_separate_from_a_new_draft() {
        let mut app = App::new("workspace".into());
        app.add_session(UiSessionId(1), &snapshot(vec![]), true);
        let entry = accepted_turn_entry(1, "failed message");
        let _ = app.reduce(AppEvent::TurnAccepted {
            id: UiSessionId(1),
            entry: &entry,
            text: "failed message".into(),
            queue_runtime: true,
        });
        let _ = app.reduce(AppEvent::RuntimeAttached {
            id: UiSessionId(1),
            snapshot: &snapshot(vec![]),
        });
        assert!(matches!(
            app.on_event(Event::Paste("draft written while opening".into())),
            Action::DraftChanged {
                id: UiSessionId(1),
                ..
            }
        ));

        let _ = app.reduce(AppEvent::RuntimeStartFailed {
            id: UiSessionId(1),
            reason: "connection closed before receipt".into(),
        });

        assert_eq!(app.pending_post(UiSessionId(1)), Some("failed message"));
        assert_eq!(
            app.current().conversation.composer.lines(),
            ["draft written while opening"],
            "the next draft remains editable instead of being merged into the durable retry"
        );
        assert_eq!(
            app.current().state,
            SessionState::Detached("connection closed before receipt".into())
        );
    }

    #[test]
    fn durable_turn_acceptance_projects_the_message_and_clears_only_in_the_reducer() {
        let mut app = App::new("workspace".into());
        app.add_session(UiSessionId(1), &snapshot(vec![]), true);
        app.on_event(Event::Paste("make this durable".into()));
        let draft = app.current().conversation.draft();

        assert!(matches!(
            app.on_event(Event::Key(key(
                KeyCode::Enter,
                KeyModifiers::NONE,
                KeyEventKind::Press,
            ))),
            Action::Post { id: UiSessionId(1), text } if text == "make this durable"
        ));
        // The terminal intent does not claim success. The draft remains until
        // the durable effect sends a TurnAccepted event back through reduce.
        assert_eq!(
            app.current().conversation.composer.lines(),
            ["make this durable"]
        );
        assert_eq!(
            app.sessions[0].draft_persistence,
            DraftPersistenceState::Pending
        );

        let _ = app.reduce(AppEvent::TurnRejected {
            id: UiSessionId(1),
            reason: "journal is temporarily locked".into(),
        });
        assert_eq!(
            app.current().conversation.composer.lines(),
            ["make this durable"],
            "a rejected durable write never clears user input"
        );
        assert_eq!(app.notice(), Some("journal is temporarily locked"));

        let entry = accepted_turn_entry(1, "make this durable");
        let _ = app.reduce(AppEvent::TurnAccepted {
            id: UiSessionId(1),
            entry: &entry,
            text: "make this durable".into(),
            queue_runtime: true,
        });

        assert_eq!(app.current().conversation.composer.lines(), [""]);
        assert_eq!(
            app.sessions[0].draft_persistence,
            DraftPersistenceState::Saved
        );
        assert_eq!(
            app.sessions[0]
                .conversation
                .projection
                .timeline
                .last()
                .map(|item| &item.kind),
            Some(&TimelineKind::Message {
                speaker: Speaker::User,
                text: "make this durable".into(),
            })
        );
        assert_eq!(
            app.sessions[0].state,
            SessionState::Opening,
            "queueing a runtime is also reducer-owned state"
        );
        assert_eq!(app.pending_post(UiSessionId(1)), Some("make this durable"));

        // A stale persistence result for the draft captured before acceptance
        // cannot resurrect an already-cleared composer state.
        let _ = app.reduce(AppEvent::DraftPersisted {
            id: UiSessionId(1),
            draft,
        });
        assert_eq!(
            app.sessions[0].draft_persistence,
            DraftPersistenceState::Saved
        );
    }

    #[test]
    fn draft_persistence_results_are_reducer_owned_and_ignore_stale_drafts() {
        let mut app = App::new("workspace".into());
        app.add_session(UiSessionId(1), &snapshot(vec![]), true);

        app.on_event(Event::Paste("first".into()));
        let first = app.current().conversation.draft();
        assert_eq!(
            app.sessions[0].draft_persistence,
            DraftPersistenceState::Pending
        );
        app.on_event(Event::Paste(" second".into()));
        let current = app.current().conversation.draft();

        let _ = app.reduce(AppEvent::DraftPersisted {
            id: UiSessionId(1),
            draft: first.clone(),
        });
        assert_eq!(
            app.sessions[0].draft_persistence,
            DraftPersistenceState::Pending,
            "an older write cannot mark newer text as saved"
        );

        let _ = app.reduce(AppEvent::DraftPersistenceFailed {
            id: UiSessionId(1),
            draft: first,
            reason: "old storage attempt failed".into(),
        });
        assert_eq!(app.notice(), None, "a stale failure is not misleading");

        let _ = app.reduce(AppEvent::DraftPersistenceFailed {
            id: UiSessionId(1),
            draft: current.clone(),
            reason: "disk full".into(),
        });
        assert_eq!(
            app.sessions[0].draft_persistence,
            DraftPersistenceState::Failed("disk full".into())
        );
        assert_eq!(
            app.notice(),
            Some("Draft is still on screen but was not saved: disk full")
        );

        let _ = app.reduce(AppEvent::DraftPersisted {
            id: UiSessionId(1),
            draft: current,
        });
        assert_eq!(
            app.sessions[0].draft_persistence,
            DraftPersistenceState::Saved
        );
    }

    #[test]
    fn connection_and_runtime_outcomes_only_change_state_via_events() {
        let mut app = App::new("workspace".into());
        app.add_session(UiSessionId(1), &snapshot(vec![]), true);
        let entry = accepted_turn_entry(1, "retry me");
        let _ = app.reduce(AppEvent::TurnAccepted {
            id: UiSessionId(1),
            entry: &entry,
            text: "retry me".into(),
            queue_runtime: true,
        });

        let _ = app.reduce(AppEvent::ConnectionStarting);
        assert_eq!(app.connection, ConnectionState::Connecting);

        let _ = app.reduce(AppEvent::ConnectionFailed {
            reason: "authentication expired".into(),
            affected: vec![UiSessionId(1)],
        });
        assert_eq!(
            app.connection,
            ConnectionState::Failed("authentication expired".into())
        );
        assert_eq!(
            app.sessions[0].state,
            SessionState::Detached("authentication expired".into())
        );
        assert_eq!(app.pending_post(UiSessionId(1)), Some("retry me"));

        let _ = app.reduce(AppEvent::ConnectionSucceeded);
        assert_eq!(app.connection, ConnectionState::Connected);
        let _ = app.reduce(AppEvent::RuntimeStartQueued {
            id: UiSessionId(1),
            text: "retry me".into(),
        });
        assert_eq!(app.sessions[0].state, SessionState::Opening);

        let _ = app.reduce(AppEvent::RuntimeAttached {
            id: UiSessionId(1),
            snapshot: &snapshot(vec![]),
        });
        assert_eq!(app.sessions[0].state, SessionState::Live);
        assert_eq!(app.pending_post(UiSessionId(1)), Some("retry me"));

        let _ = app.reduce(AppEvent::PendingPostAcknowledged { id: UiSessionId(1) });
        assert_eq!(app.pending_post(UiSessionId(1)), None);
    }

    #[test]
    fn model_readiness_refresh_only_changes_model_derived_detached_states() {
        let mut app = App::new("workspace".into());
        app.add_session(UiSessionId(1), &snapshot(vec![]), true);
        app.sessions[0].state = SessionState::NeedsSetup("choose a model".into());

        let _ = app.reduce(AppEvent::SessionModelReadiness {
            id: UiSessionId(1),
            ready: true,
        });
        assert!(matches!(app.sessions[0].state, SessionState::Detached(_)));

        app.sessions[0].state = SessionState::Live;
        let _ = app.reduce(AppEvent::SessionModelReadiness {
            id: UiSessionId(1),
            ready: false,
        });
        assert_eq!(app.sessions[0].state, SessionState::Live);
    }

    #[test]
    fn session_focus_selects_and_returns_typed_input_to_the_composer() {
        let mut app = App::new("workspace".into());
        app.add_session(UiSessionId(1), &snapshot(vec![user(1, "first")]), true);
        app.add_session(UiSessionId(2), &snapshot(vec![user(2, "second")]), true);
        app.sessions[0].background_unread = true;
        app.set_viewport(Viewport {
            width: 80,
            height: 20,
            spacing: 1,
        });

        app.on_event(Event::Key(key(
            KeyCode::Left,
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        )));
        assert_eq!(app.focus, Focus::Sessions);

        app.on_event(Event::Key(key(
            KeyCode::Up,
            KeyModifiers::NONE,
            KeyEventKind::Press,
        )));
        assert_eq!(app.current().id, UiSessionId(1));
        assert!(app.current().background_unread);

        app.on_event(Event::Key(key(
            KeyCode::Char('a'),
            KeyModifiers::NONE,
            KeyEventKind::Press,
        )));
        assert_eq!(app.focus, Focus::Composer);
        assert!(!app.current().background_unread);
        assert_eq!(app.current().conversation.composer.lines(), ["a"]);

        app.on_event(Event::Key(key(
            KeyCode::Left,
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        )));
        app.on_event(Event::Paste("bc".into()));
        assert_eq!(app.focus, Focus::Composer);
        assert_eq!(app.current().conversation.composer.lines(), ["abc"]);

        app.on_event(Event::Key(key(
            KeyCode::Left,
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        )));
        app.on_event(Event::Key(key(
            KeyCode::Backspace,
            KeyModifiers::NONE,
            KeyEventKind::Press,
        )));
        assert_eq!(app.focus, Focus::Composer);
        assert_eq!(app.current().conversation.composer.lines(), ["ab"]);

        app.on_event(Event::Key(key(
            KeyCode::Left,
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        )));
        app.on_event(Event::Key(key(
            KeyCode::Right,
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        )));
        assert_eq!(app.focus, Focus::Composer);
        app.on_event(Event::Paste(" kept".into()));
        assert_eq!(app.current().conversation.composer.lines(), ["ab kept"]);

        app.on_event(Event::Key(key(
            KeyCode::Left,
            KeyModifiers::NONE,
            KeyEventKind::Press,
        )));
        app.on_event(Event::Key(key(
            KeyCode::Left,
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        )));
        app.on_event(Event::Key(key(
            KeyCode::Delete,
            KeyModifiers::NONE,
            KeyEventKind::Press,
        )));
        assert_eq!(app.focus, Focus::Composer);
        assert_eq!(app.current().conversation.composer.lines(), ["ab kep"]);
    }

    #[test]
    fn narrow_view_still_enters_the_full_screen_session_list() {
        let mut app = App::new("workspace".into());
        app.add_session(UiSessionId(1), &snapshot(vec![user(1, "first")]), true);
        app.add_session(UiSessionId(2), &snapshot(vec![user(2, "second")]), true);
        app.set_viewport(Viewport {
            width: 40,
            height: 12,
            spacing: 1,
        });

        for (code, modifiers) in [
            (KeyCode::Up, KeyModifiers::CONTROL),
            (KeyCode::Up, KeyModifiers::ALT),
            (KeyCode::Char('1'), KeyModifiers::ALT),
        ] {
            app.on_event(Event::Key(key(code, modifiers, KeyEventKind::Press)));
            assert_eq!(app.current().id, UiSessionId(2));
        }

        app.on_event(Event::Key(key(
            KeyCode::Left,
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        )));
        assert_eq!(app.focus, Focus::Sessions);
        for modifiers in [KeyModifiers::CONTROL, KeyModifiers::ALT] {
            app.on_event(Event::Key(key(KeyCode::Up, modifiers, KeyEventKind::Press)));
            assert_eq!(app.current().id, UiSessionId(2));
        }
        app.on_event(Event::Key(key(
            KeyCode::Up,
            KeyModifiers::NONE,
            KeyEventKind::Press,
        )));
        assert_eq!(app.current().id, UiSessionId(1));
        app.on_event(Event::Key(key(
            KeyCode::Right,
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        )));
        assert_eq!(app.focus, Focus::Composer);
    }

    #[test]
    fn control_right_in_the_composer_is_forwarded_to_the_textarea() {
        let mut app = App::new("workspace".into());
        app.add_session(UiSessionId(1), &snapshot(vec![]), true);
        app.on_event(Event::Paste("one two".into()));
        app.on_event(Event::Key(key(
            KeyCode::Home,
            KeyModifiers::NONE,
            KeyEventKind::Press,
        )));
        assert_eq!(app.current().conversation.composer.cursor(), (0, 0));

        app.on_event(Event::Key(key(
            KeyCode::Right,
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        )));

        assert_eq!(app.focus, Focus::Composer);
        assert!(app.current().conversation.composer.cursor().1 > 0);
    }

    #[test]
    fn a_long_single_reply_scrolls_by_visual_rows() {
        let mut app = App::new("workspace".into());
        app.add_session(
            UiSessionId(1),
            &snapshot(vec![reply(1, &"很长的中文回复".repeat(60))]),
            true,
        );
        app.set_viewport(Viewport {
            width: 12,
            height: 5,
            spacing: 0,
        });

        for _ in 0..3 {
            app.on_event(Event::Key(key(
                KeyCode::PageUp,
                KeyModifiers::NONE,
                KeyEventKind::Press,
            )));
        }

        let anchor = app.current().conversation.anchor.unwrap();
        assert_eq!(anchor.cursor, 1);
        assert!(
            anchor.line > 0,
            "the viewport should reach inside the reply"
        );
        let middle = anchor.line;
        app.on_event(Event::Key(key(
            KeyCode::PageDown,
            KeyModifiers::NONE,
            KeyEventKind::Press,
        )));
        assert!(app.current().conversation.anchor.unwrap().line > middle);
    }

    #[test]
    fn page_up_on_short_history_keeps_following_the_live_tail() {
        let mut app = App::new("workspace".into());
        app.add_session(UiSessionId(1), &snapshot(vec![reply(1, "short")]), true);
        app.set_viewport(Viewport {
            width: 40,
            height: 10,
            spacing: 1,
        });

        app.on_event(Event::Key(key(
            KeyCode::PageUp,
            KeyModifiers::NONE,
            KeyEventKind::Press,
        )));
        assert_eq!(app.current().conversation.anchor, None);

        app.apply(UiSessionId(1), &step(vec![reply(2, "new reply")]));
        assert_eq!(app.current().conversation.anchor, None);
        assert!(!app.current().conversation.unread);
    }

    #[test]
    fn model_jobs_stay_in_activity_instead_of_the_timeline() {
        let records = vec![RecordEntry {
            cursor: 1,
            kind: RecordKind::Notice(Notice::JobStarted {
                id: bone_agent::JobId(1),
                request: bone_agent::JobRequest::Work { messages: vec![] },
            }),
        }];
        let projection = Projection::from_snapshot(&snapshot(records), true);
        assert!(projection.timeline.is_empty());
        assert_eq!(projection.activity().as_deref(), Some("Thinking"));
    }

    #[test]
    fn finishing_pure_reasoning_settles_into_waiting() {
        let mut state = snapshot(vec![
            RecordEntry {
                cursor: 1,
                kind: RecordKind::Notice(Notice::JobStarted {
                    id: bone_agent::JobId(1),
                    request: bone_agent::JobRequest::Work { messages: vec![] },
                }),
            },
            RecordEntry {
                cursor: 2,
                kind: RecordKind::Notice(Notice::JobFinished {
                    id: bone_agent::JobId(1),
                    outcome: JobOutcome::work(Default::default()),
                }),
            },
        ]);
        state.autonomous = true;

        let projection = Projection::from_snapshot(&state, true);
        assert_eq!(projection.status, SessionStatus::Waiting);
        assert!(projection.active.is_empty());
        assert_eq!(projection.activity(), None);
    }

    #[test]
    fn activity_uses_the_latest_progress_message_without_growing_history() {
        let records = vec![
            tool_started(1, 1, "read"),
            RecordEntry {
                cursor: 2,
                kind: RecordKind::Notice(Notice::JobProgress {
                    id: bone_agent::JobId(1),
                    progress: bone_agent::JobProgress {
                        message: "opening files".into(),
                        percent: None,
                    },
                }),
            },
            RecordEntry {
                cursor: 3,
                kind: RecordKind::Notice(Notice::JobProgress {
                    id: bone_agent::JobId(1),
                    progress: bone_agent::JobProgress {
                        message: "reading\nworkspace".into(),
                        percent: Some(42),
                    },
                }),
            },
        ];

        let projection = Projection::from_snapshot(&snapshot(records.clone()), true);
        assert_eq!(
            projection.activity().as_deref(),
            Some("Reading file 42% · reading workspace")
        );
        assert!(projection.timeline.is_empty());
        assert_eq!(
            Projection::from_snapshot(&snapshot(records), false).activity(),
            None
        );
    }

    fn key(code: KeyCode, modifiers: KeyModifiers, kind: KeyEventKind) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind,
            state: KeyEventState::NONE,
        }
    }

    fn snapshot(record: Vec<RecordEntry>) -> Snapshot {
        Snapshot {
            record_cursor: record.last().map_or(0, |entry| entry.cursor),
            revision: 0,
            generation: 0,
            requirement: None,
            autonomous: false,
            work: None,
            review: None,
            candidate: None,
            pending_messages: vec![],
            jobs: vec![],
            record,
            tools: vec![],
        }
    }

    fn accepted_turn_entry(sequence: u64, text: &str) -> JournalEntry {
        assert_eq!(sequence, 1, "test helper only needs the first journal fact");
        JournalEntry {
            sequence: JournalSequence::first(),
            occurred_at: UnixMillis::from_millis(1).expect("epoch timestamp is valid"),
            fact: JournalFact::UserTurnAccepted {
                turn: sequence,
                text: text.into(),
                runtime_fingerprint: "test-fingerprint".into(),
                solver_model: "test-model".into(),
            },
        }
    }

    fn conversation_records() -> Vec<RecordEntry> {
        vec![
            user(1, "Compare A and B"),
            RecordEntry {
                cursor: 2,
                kind: RecordKind::Notice(Notice::JobStarted {
                    id: bone_agent::JobId(1),
                    request: bone_agent::JobRequest::Work {
                        messages: vec![MessageId(1)],
                    },
                }),
            },
            reply(3, "I will compare them."),
            RecordEntry {
                cursor: 4,
                kind: RecordKind::Notice(Notice::Finished { cleanup: vec![] }),
            },
        ]
    }

    fn user(cursor: u64, text: &str) -> RecordEntry {
        RecordEntry {
            cursor,
            kind: RecordKind::UserMessage(Message {
                id: MessageId(cursor),
                text: text.into(),
            }),
        }
    }

    fn reply(cursor: u64, text: &str) -> RecordEntry {
        RecordEntry {
            cursor,
            kind: RecordKind::Notice(Notice::Reply {
                text: text.into(),
                reply_to: vec![],
                as_of: cursor,
            }),
        }
    }

    fn tool_started(cursor: u64, id: u64, name: &str) -> RecordEntry {
        RecordEntry {
            cursor,
            kind: RecordKind::Notice(Notice::JobStarted {
                id: bone_agent::JobId(id),
                request: bone_agent::JobRequest::Tool(ToolCall::new(name, serde_json::json!({}))),
            }),
        }
    }

    fn tool_finished(cursor: u64, id: u64, outcome: JobOutcome) -> RecordEntry {
        RecordEntry {
            cursor,
            kind: RecordKind::Notice(Notice::JobFinished {
                id: bone_agent::JobId(id),
                outcome,
            }),
        }
    }

    fn step(records: Vec<RecordEntry>) -> StepEvent {
        StepEvent {
            sequence: 1,
            elapsed: Duration::ZERO,
            event: AgentEvent::Stop,
            records,
            effects: Vec::<EffectSummary>::new(),
        }
    }

    #[test]
    fn message_kind_carries_the_expected_speaker() {
        let projection = Projection::from_snapshot(&snapshot(vec![user(1, "hello")]), true);
        assert!(matches!(
            projection.timeline[0].kind,
            TimelineKind::Message {
                speaker: Speaker::User,
                ..
            }
        ));
    }
}
