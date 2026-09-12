use std::collections::BTreeMap;

use bone_app::SessionId;

use crate::editor::{EditCommand, EditorBuffer};

/// Session-scoped title state. Durable operations deliberately live outside
/// `SessionUi` so release, reopen, and temporarily missing overview rows cannot
/// discard their identities or optimistic values.
#[derive(Debug, Default)]
pub(super) struct TitleState {
    edit: Option<TitleEdit>,
    sessions: BTreeMap<SessionId, SessionTitleState>,
    next_request: u64,
}

#[derive(Debug, Default)]
struct SessionTitleState {
    /// A successful local write kept until an authoritative read observes it.
    confirmed: Option<String>,
    /// More than one request is possible during graceful exit, when the final
    /// queued edit is forced behind an already running write.
    manual_pending: BTreeMap<u64, String>,
    /// Highest successful manual request reflected in UI state. Runtime writes
    /// are serialized, but their observer tasks may enqueue receipts in reverse.
    manual_applied: u64,
    manual_queued: Option<String>,
    /// Once set, automatic naming may complete but can no longer change title
    /// state during this process.
    manual_intent: bool,
    /// Generation is retained for failure visibility; request is the identity.
    auto_pending: BTreeMap<u64, u64>,
    /// Suppresses an older failure after a newer automatic request has already
    /// completed. Successful older results may still carry the only generated
    /// title because runtime writes are serialized and later requests can be no-ops.
    auto_completed: u64,
}

impl SessionTitleState {
    fn desired(&self) -> Option<&str> {
        self.manual_queued
            .as_deref()
            .or_else(|| {
                self.manual_pending
                    .last_key_value()
                    .map(|(_, title)| title.as_str())
            })
            .or(self.confirmed.as_deref())
    }

    fn manual_pending(&self) -> bool {
        !self.manual_pending.is_empty() || self.manual_queued.is_some()
    }

    fn is_disposable(&self) -> bool {
        !self.manual_intent
            && self.confirmed.is_none()
            && self.manual_pending.is_empty()
            && self.manual_queued.is_none()
            && self.auto_pending.is_empty()
    }
}

#[derive(Debug)]
struct TitleEdit {
    target: SessionId,
    original: String,
    editor: EditorBuffer,
    /// Explicitly records whether a late automatic title may initialize this
    /// editor. User text changes and cancellation permanently close that path.
    accept_auto_rebase: bool,
}

impl TitleEdit {
    fn new(target: SessionId, title: String) -> Self {
        Self {
            target,
            original: title.clone(),
            editor: EditorBuffer::new(title),
            accept_auto_rebase: true,
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(super) struct TitleWrite {
    pub(super) session: SessionId,
    pub(super) request: u64,
    pub(super) title: String,
}

#[derive(Debug, Eq, PartialEq)]
pub(super) struct AutoTitleWrite {
    pub(super) session: SessionId,
    pub(super) request: u64,
    pub(super) first_input: String,
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum TitleCommit {
    Invalid,
    Accepted(Option<TitleWrite>),
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum ManualTitleSettlement {
    Ignored,
    Finished {
        row_title: Option<String>,
        next_write: Option<TitleWrite>,
        final_error: Option<String>,
        drained: bool,
    },
}

#[derive(Debug, Eq, PartialEq)]
pub(super) struct AutoTitleFailure {
    pub(super) generation: u64,
    pub(super) message: String,
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum AutoTitleSettlement {
    Ignored,
    Finished {
        row_title: Option<String>,
        failure: Option<AutoTitleFailure>,
    },
}

impl TitleState {
    pub(super) fn begin_edit(&mut self, target: SessionId, title: String) {
        if self.edit.as_ref().map(|edit| edit.target) != Some(target) {
            self.edit = Some(TitleEdit::new(target, title));
        }
    }

    pub(super) fn edit_target(&self) -> Option<SessionId> {
        self.edit.as_ref().map(|edit| edit.target)
    }

    pub(super) fn editor(&self, session: SessionId) -> Option<&EditorBuffer> {
        self.edit
            .as_ref()
            .filter(|edit| edit.target == session)
            .map(|edit| &edit.editor)
    }

    pub(super) fn history_editor_mut(&mut self) -> Option<&mut EditorBuffer> {
        self.edit.as_mut().map(|edit| &mut edit.editor)
    }

    pub(super) fn apply_edit(&mut self, command: EditCommand) -> bool {
        let edit = self
            .edit
            .as_mut()
            .expect("title editor is prepared before title input");
        let changed = edit.editor.apply(command);
        if changed {
            edit.accept_auto_rebase = false;
        }
        changed
    }

    pub(super) fn replace_user(&mut self, text: String, cursor: usize) {
        let edit = self
            .edit
            .as_mut()
            .expect("title editor is prepared before replacing its text");
        let revision = edit.editor.revision();
        edit.editor.replace_user(text, cursor);
        if edit.editor.revision() != revision {
            edit.accept_auto_rebase = false;
        }
    }

    pub(super) fn break_interaction(&mut self) {
        if let Some(edit) = &mut self.edit {
            edit.editor.break_interaction();
        }
    }

    pub(super) fn clear_edit(&mut self) {
        self.edit = None;
    }

    pub(super) fn desired(&self, session: SessionId) -> Option<&str> {
        self.sessions
            .get(&session)
            .and_then(SessionTitleState::desired)
    }

    pub(super) fn manual_pending(&self, session: SessionId) -> bool {
        self.sessions
            .get(&session)
            .is_some_and(SessionTitleState::manual_pending)
    }

    pub(super) fn manual_intent(&self, session: SessionId) -> bool {
        self.sessions
            .get(&session)
            .is_some_and(|title| title.manual_intent)
    }

    pub(super) fn cancel_edit(&mut self, row_title: Option<&str>) {
        let Some(target) = self.edit_target() else {
            return;
        };
        let fallback = self.intended_title(target, row_title);
        let edit = self.edit.as_mut().expect("title target was read above");
        let text = fallback.unwrap_or_else(|| edit.original.clone());
        edit.original = text.clone();
        edit.accept_auto_rebase = false;
        let revision = edit.editor.revision().wrapping_add(1);
        let cursor = text.len();
        edit.editor.reset_external(text, cursor, revision);
    }

    pub(super) fn commit(&mut self, row_title: Option<&str>) -> Option<TitleCommit> {
        let target = self.edit_target()?;
        let fallback = self.intended_title(target, row_title);
        let title = {
            let edit = self.edit.as_mut().expect("title target was read above");
            let title = edit.editor.text().trim().to_owned();
            if title.is_empty() || title.len() > 200 {
                let text = fallback.unwrap_or_else(|| edit.original.clone());
                edit.original = text.clone();
                edit.accept_auto_rebase = false;
                let revision = edit.editor.revision().wrapping_add(1);
                let cursor = text.len();
                edit.editor.reset_external(text, cursor, revision);
                return Some(TitleCommit::Invalid);
            }
            if edit.editor.text() != title {
                edit.accept_auto_rebase = false;
                let revision = edit.editor.revision().wrapping_add(1);
                edit.editor
                    .reset_external(title.clone(), title.len(), revision);
            }
            title
        };

        let committed = self.committed_title(target, row_title).unwrap_or_default();
        let session = self.sessions.entry(target).or_default();
        let current = session
            .manual_pending
            .last_key_value()
            .map_or(committed.as_str(), |(_, pending)| pending.as_str());
        let changed = current != title;
        session.manual_queued = changed.then_some(title);
        if changed {
            session.manual_intent = true;
        }
        let write = self.dispatch_queued(target, row_title);
        self.remove_if_disposable(target);
        Some(TitleCommit::Accepted(write))
    }

    pub(super) fn finish_manual(
        &mut self,
        session: SessionId,
        request: u64,
        result: Result<String, String>,
        row_title: Option<&str>,
    ) -> ManualTitleSettlement {
        let Some(queue) = self.sessions.get_mut(&session) else {
            return ManualTitleSettlement::Ignored;
        };
        let Some(completed) = queue.manual_pending.remove(&request) else {
            return ManualTitleSettlement::Ignored;
        };

        let mut published = None;
        let mut final_error = None;
        match result {
            Ok(title) => {
                if request > queue.manual_applied {
                    queue.manual_applied = request;
                    queue.confirmed = Some(title.clone());
                    published = Some(title.clone());
                    if let Some(edit) = self.edit.as_mut().filter(|edit| edit.target == session) {
                        edit.original = title.clone();
                        if edit.editor.text() == completed {
                            let revision = edit.editor.revision();
                            let cursor = title.len();
                            edit.editor.reset_external(title, cursor, revision);
                        }
                    }
                }
            }
            Err(message) => {
                let superseded = request <= queue.manual_applied
                    || !queue.manual_pending.is_empty()
                    || queue.manual_queued.is_some();
                let next_write = self.dispatch_queued(session, row_title);
                let drained = !self.manual_pending(session);
                if !superseded && drained {
                    let committed = self.committed_title(session, row_title).unwrap_or_default();
                    if let Some(edit) = self.edit.as_mut().filter(|edit| edit.target == session) {
                        edit.original = committed.clone();
                        if edit.editor.text() == completed {
                            edit.accept_auto_rebase = false;
                            let revision = edit.editor.revision().wrapping_add(1);
                            edit.editor.reset_external(
                                committed.clone(),
                                committed.len(),
                                revision,
                            );
                        }
                    }
                    final_error = Some(message);
                }
                self.remove_if_disposable(session);
                return ManualTitleSettlement::Finished {
                    row_title: None,
                    next_write,
                    final_error,
                    drained,
                };
            }
        }

        let next_write = self.dispatch_queued(session, row_title);
        let drained = !self.manual_pending(session);
        self.remove_if_disposable(session);
        ManualTitleSettlement::Finished {
            row_title: published,
            next_write,
            final_error,
            drained,
        }
    }

    pub(super) fn start_auto(
        &mut self,
        session: SessionId,
        generation: u64,
        first_input: String,
    ) -> Option<AutoTitleWrite> {
        if self.manual_intent(session) {
            return None;
        }
        let request = self.request();
        self.sessions
            .entry(session)
            .or_default()
            .auto_pending
            .insert(request, generation);
        Some(AutoTitleWrite {
            session,
            request,
            first_input,
        })
    }

    pub(super) fn finish_auto(
        &mut self,
        session: SessionId,
        request: u64,
        result: Result<Option<String>, String>,
    ) -> AutoTitleSettlement {
        let Some(queue) = self.sessions.get_mut(&session) else {
            return AutoTitleSettlement::Ignored;
        };
        let Some(generation) = queue.auto_pending.remove(&request) else {
            return AutoTitleSettlement::Ignored;
        };
        let newer_completion_observed = request < queue.auto_completed;
        queue.auto_completed = queue.auto_completed.max(request);

        let mut row_title = None;
        let mut failure = None;
        match result {
            Ok(Some(title)) if !queue.manual_intent => {
                queue.confirmed = Some(title.clone());
                row_title = Some(title.clone());
                if !queue.manual_pending()
                    && let Some(edit) = self
                        .edit
                        .as_mut()
                        .filter(|edit| edit.target == session && edit.accept_auto_rebase)
                {
                    edit.original = title.clone();
                    edit.editor = EditorBuffer::new(title);
                }
            }
            Ok(Some(_)) | Ok(None) => {}
            Err(message) if !queue.manual_intent && !newer_completion_observed => {
                failure = Some(AutoTitleFailure {
                    generation,
                    message,
                });
            }
            Err(_) => {}
        }
        self.remove_if_disposable(session);
        AutoTitleSettlement::Finished { row_title, failure }
    }

    pub(super) fn reconcile_authoritative(
        &mut self,
        session: SessionId,
        authoritative: &str,
        current: Option<&str>,
    ) -> String {
        let (confirmation_observed, pending, confirmed) =
            self.sessions
                .get_mut(&session)
                .map_or((false, false, None), |queue| {
                    let confirmation_observed = queue.confirmed.as_deref() == Some(authoritative);
                    if confirmation_observed {
                        queue.confirmed = None;
                    }
                    (
                        confirmation_observed,
                        queue.manual_pending(),
                        queue.confirmed.clone(),
                    )
                });

        let (reconciled, update_editor) = if confirmation_observed {
            (authoritative.to_owned(), !pending)
        } else if let Some(confirmed) = confirmed {
            (confirmed, false)
        } else if pending {
            (current.unwrap_or(authoritative).to_owned(), false)
        } else {
            (authoritative.to_owned(), true)
        };

        if update_editor {
            self.update_editor_baseline(session, authoritative);
        }
        self.remove_if_disposable(session);
        reconciled
    }

    pub(super) fn flush_for_exit(&mut self) -> Vec<TitleWrite> {
        let sessions = self
            .sessions
            .iter()
            .filter_map(|(session, title)| title.manual_queued.as_ref().map(|_| *session))
            .collect::<Vec<_>>();
        let mut writes = Vec::new();
        for session in sessions {
            let title = self
                .sessions
                .get_mut(&session)
                .and_then(|title| title.manual_queued.take())
                .expect("only queues with a title were collected");
            let already_last = self
                .sessions
                .get(&session)
                .and_then(|title| title.manual_pending.last_key_value())
                .is_some_and(|(_, pending)| pending == &title);
            if !already_last {
                writes.push(self.enqueue_manual(session, title));
            }
        }
        writes
    }

    fn intended_title(&self, session: SessionId, row_title: Option<&str>) -> Option<String> {
        self.desired(session)
            .map(str::to_owned)
            .or_else(|| self.committed_title(session, row_title))
    }

    fn committed_title(&self, session: SessionId, row_title: Option<&str>) -> Option<String> {
        self.sessions
            .get(&session)
            .and_then(|title| title.confirmed.clone())
            .or_else(|| row_title.map(str::to_owned))
    }

    fn update_editor_baseline(&mut self, session: SessionId, authoritative: &str) {
        if let Some(edit) = self.edit.as_mut().filter(|edit| edit.target == session) {
            let clean = edit.editor.text() == edit.original;
            edit.original = authoritative.to_owned();
            if clean {
                let revision = edit.editor.revision();
                let cursor = edit.original.len();
                edit.editor
                    .reset_external(edit.original.clone(), cursor, revision);
            }
        }
    }

    fn dispatch_queued(
        &mut self,
        session: SessionId,
        row_title: Option<&str>,
    ) -> Option<TitleWrite> {
        let title = {
            let queue = self.sessions.get_mut(&session)?;
            if !queue.manual_pending.is_empty() {
                return None;
            }
            queue.manual_queued.take()?
        };
        if self.committed_title(session, row_title).as_deref() == Some(title.as_str()) {
            return None;
        }
        Some(self.enqueue_manual(session, title))
    }

    fn enqueue_manual(&mut self, session: SessionId, title: String) -> TitleWrite {
        let request = self.request();
        self.sessions
            .entry(session)
            .or_default()
            .manual_pending
            .insert(request, title.clone());
        TitleWrite {
            session,
            request,
            title,
        }
    }

    fn request(&mut self) -> u64 {
        self.next_request = self.next_request.wrapping_add(1).max(1);
        self.next_request
    }

    fn remove_if_disposable(&mut self, session: SessionId) {
        if self
            .sessions
            .get(&session)
            .is_some_and(SessionTitleState::is_disposable)
        {
            self.sessions.remove(&session);
        }
    }

    #[cfg(test)]
    pub(super) fn edit_original(&self) -> Option<&str> {
        self.edit.as_ref().map(|edit| edit.original.as_str())
    }

    #[cfg(test)]
    pub(super) fn edit_accepts_auto_rebase(&self) -> Option<bool> {
        self.edit.as_ref().map(|edit| edit.accept_auto_rebase)
    }

    #[cfg(test)]
    pub(super) fn confirmed(&self, session: SessionId) -> Option<&str> {
        self.sessions
            .get(&session)
            .and_then(|title| title.confirmed.as_deref())
    }

    #[cfg(test)]
    pub(super) fn manual_request_pending(&self, session: SessionId, request: u64) -> bool {
        self.sessions
            .get(&session)
            .is_some_and(|title| title.manual_pending.contains_key(&request))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> SessionId {
        SessionId::new()
    }

    #[test]
    fn every_auto_outcome_retires_only_its_exact_request() {
        let id = session();
        let mut titles = TitleState::default();
        let changed = titles.start_auto(id, 7, "first".into()).unwrap();
        let unchanged = titles.start_auto(id, 8, "second".into()).unwrap();
        let failed = titles.start_auto(id, 9, "third".into()).unwrap();

        assert_eq!(
            titles.finish_auto(id, changed.request, Ok(Some("Automatic title".into()))),
            AutoTitleSettlement::Finished {
                row_title: Some("Automatic title".into()),
                failure: None,
            }
        );
        assert_eq!(
            titles.finish_auto(id, unchanged.request, Ok(None)),
            AutoTitleSettlement::Finished {
                row_title: None,
                failure: None,
            }
        );
        assert_eq!(
            titles.finish_auto(id, failed.request, Err("failed".into())),
            AutoTitleSettlement::Finished {
                row_title: None,
                failure: Some(AutoTitleFailure {
                    generation: 9,
                    message: "failed".into(),
                }),
            }
        );
        assert_eq!(
            titles.finish_auto(id, failed.request, Ok(None)),
            AutoTitleSettlement::Ignored
        );
        assert_eq!(
            titles.finish_auto(session(), changed.request, Ok(None)),
            AutoTitleSettlement::Ignored
        );
    }

    #[test]
    fn manual_intent_retires_but_cannot_apply_late_auto_title() {
        let id = session();
        let mut titles = TitleState::default();
        titles.begin_edit(id, "Old".into());
        let auto = titles.start_auto(id, 4, "first".into()).unwrap();
        titles.replace_user("Manual".into(), 6);
        assert!(matches!(
            titles.commit(Some("Old")),
            Some(TitleCommit::Accepted(Some(_)))
        ));

        assert_eq!(
            titles.finish_auto(id, auto.request, Ok(Some("Automatic".into()))),
            AutoTitleSettlement::Finished {
                row_title: None,
                failure: None,
            }
        );
        assert_eq!(titles.editor(id).unwrap().text(), "Manual");
    }

    #[test]
    fn manual_intent_silences_a_late_auto_title_failure() {
        let id = session();
        let mut titles = TitleState::default();
        titles.begin_edit(id, "Old".into());
        let auto = titles.start_auto(id, 4, "first".into()).unwrap();
        titles.replace_user("Manual".into(), 6);
        assert!(matches!(
            titles.commit(Some("Old")),
            Some(TitleCommit::Accepted(Some(_)))
        ));

        assert_eq!(
            titles.finish_auto(id, auto.request, Err("obsolete failure".into())),
            AutoTitleSettlement::Finished {
                row_title: None,
                failure: None,
            }
        );
    }

    #[test]
    fn older_auto_failure_is_silent_after_a_newer_completion() {
        let id = session();
        let mut titles = TitleState::default();
        let older = titles.start_auto(id, 4, "first".into()).unwrap();
        let newer = titles.start_auto(id, 4, "second".into()).unwrap();

        assert_eq!(
            titles.finish_auto(id, newer.request, Ok(None)),
            AutoTitleSettlement::Finished {
                row_title: None,
                failure: None,
            }
        );
        assert_eq!(
            titles.finish_auto(id, older.request, Err("obsolete failure".into())),
            AutoTitleSettlement::Finished {
                row_title: None,
                failure: None,
            }
        );
    }

    #[test]
    fn cancel_explicitly_closes_auto_rebase_without_using_editor_revision() {
        let id = session();
        let mut titles = TitleState::default();
        titles.begin_edit(id, "Old".into());
        let auto = titles.start_auto(id, 1, "first".into()).unwrap();
        titles.cancel_edit(Some("Old"));
        assert_eq!(titles.edit_accepts_auto_rebase(), Some(false));

        let result = titles.finish_auto(id, auto.request, Ok(Some("Automatic".into())));
        assert!(matches!(
            result,
            AutoTitleSettlement::Finished {
                row_title: Some(_),
                failure: None
            }
        ));
        assert_eq!(titles.editor(id).unwrap().text(), "Old");
    }
}
