//! Pure answer/recovery decisions. Product effects remain in the reducer/runtime.
//! Answer text must never fall through to an ordinary SubmitInput on expiry.
use std::collections::BTreeMap;

use bone_app::{
    HistoryEntry, InputId, InputState, InputView, QuestionId, RequestId, RuntimeId, RuntimeState,
    SessionEvent, SessionView, SubmitInput,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnswerDraft {
    pub question: QuestionId,
    pub text: String,
    pub cursor: usize,
    pub revision: u64,
    pub editor: crate::editor::EditorState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AnswerError {
    Empty,
    QuestionExpired,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ActiveQuestion<'a> {
    pub id: QuestionId,
    pub text: &'a str,
}

impl AnswerDraft {
    pub fn new(question: QuestionId) -> Self {
        Self {
            question,
            text: String::new(),
            cursor: 0,
            revision: 0,
            editor: Default::default(),
        }
    }

    /// Replace only this answer buffer, keeping the cursor on a grapheme boundary.
    pub fn replace(&mut self, text: String, cursor: usize) {
        if self.text != text {
            self.editor.checkpoint(&self.text, self.cursor);
        }
        self.cursor = crate::editor::floor_grapheme_boundary(&text, cursor);
        if self.text != text {
            self.revision = self.revision.wrapping_add(1);
            self.text = text;
        }
    }

    /// The caller owns request-id reuse. Even slash-prefixed answers stay answers.
    pub fn submission(
        &self,
        snapshot: &SessionView,
        request_id: RequestId,
    ) -> Result<SubmitInput, AnswerError> {
        if active_question(snapshot, self.question).is_none() {
            return Err(AnswerError::QuestionExpired);
        }
        if self.text.trim().is_empty() {
            return Err(AnswerError::Empty);
        }
        let mut input = SubmitInput::new(self.text.clone()).answer(self.question);
        input.request_id = request_id;
        Ok(input)
    }

    /// Call only after matching the receipt's request_id to its pending submission.
    /// A receipt for an old question/revision cannot clear a newer answer.
    pub fn clear_if_submitted(&mut self, question: QuestionId, revision: u64, text: &str) -> bool {
        if self.question != question || self.revision != revision || self.text != text {
            return false;
        }
        self.editor.checkpoint(&self.text, self.cursor);
        self.text.clear();
        self.cursor = 0;
        self.revision = self.revision.wrapping_add(1);
        true
    }
}

fn running_runtime(snapshot: &SessionView) -> Option<RuntimeId> {
    match &snapshot.runtime {
        RuntimeState::Running { id, .. } => Some(*id),
        _ => None,
    }
}

fn matching_question(
    inputs: &[InputView],
    runtime: Option<RuntimeId>,
    id: QuestionId,
) -> Option<ActiveQuestion<'_>> {
    if runtime != Some(id.runtime) {
        return None;
    }
    inputs.iter().find_map(|input| match &input.state {
        InputState::WaitingForUser {
            runtime,
            question,
            text,
        } if *runtime == id.runtime && *question == id && input.id == id.reply_to => {
            Some(ActiveQuestion { id, text })
        }
        _ => None,
    })
}

pub fn active_question(snapshot: &SessionView, id: QuestionId) -> Option<ActiveQuestion<'_>> {
    matching_question(&snapshot.inputs, running_runtime(snapshot), id)
}

pub fn active_questions(snapshot: &SessionView) -> Vec<ActiveQuestion<'_>> {
    snapshot
        .inputs
        .iter()
        .filter_map(|input| match &input.state {
            InputState::WaitingForUser { question, .. } => active_question(snapshot, *question),
            _ => None,
        })
        .collect()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecoveryCandidate {
    /// Use Session::retry(input), not a fresh SubmitInput.
    Retry { input: InputId },
    /// Restore through an explicit UI action. Keep reply_to for answers, including
    /// expired answers; only explicit conversion may remove that association.
    Restore {
        input: InputId,
        text: String,
        reply_to: Option<QuestionId>,
    },
}

pub fn recovery_candidate(input: &InputView) -> Option<RecoveryCandidate> {
    match &input.state {
        InputState::Queued { .. } | InputState::RoutingFailed { .. } => {
            Some(RecoveryCandidate::Retry { input: input.id })
        }
        InputState::Cancelled | InputState::Rejected { .. } | InputState::Interrupted { .. } => {
            Some(RecoveryCandidate::Restore {
                input: input.id,
                text: input.text.clone(),
                reply_to: input.reply_to,
            })
        }
        _ => None,
    }
}

/// Cancelled inputs disappear from SessionView.inputs. Recover their original
/// text from typed history events, not display strings or guessed runtime state.
/// `history` must be ascending session history (e.g. SessionUi.history.iter()).
/// Missing InputSubmitted means no restore candidate until earlier history loads.
/// Only live snapshot entries can offer Retry; partial history cannot invent it.
pub fn recoverable_inputs<'a>(
    snapshot: &SessionView,
    history: impl IntoIterator<Item = &'a HistoryEntry>,
) -> Vec<RecoveryCandidate> {
    let mut originals = BTreeMap::<InputId, (String, Option<QuestionId>)>::new();
    let mut recoverable = BTreeMap::<InputId, bool>::new();
    for entry in history {
        match &entry.event {
            SessionEvent::InputSubmitted {
                input,
                text,
                reply_to,
                ..
            } => {
                originals.insert(*input, (text.clone(), *reply_to));
            }
            SessionEvent::InputCancelled { input } | SessionEvent::InputRejected { input, .. } => {
                recoverable.insert(*input, true);
            }
            SessionEvent::Interrupted { inputs, .. } => {
                for input in inputs {
                    recoverable.insert(*input, true);
                }
            }
            SessionEvent::InputAccepted { input, .. }
            | SessionEvent::InputFinished { input, .. } => {
                recoverable.insert(*input, false);
            }
            _ => {}
        }
    }
    let mut candidates = BTreeMap::new();
    for (input, can_restore) in recoverable {
        if can_restore && let Some((text, reply_to)) = originals.remove(&input) {
            candidates.insert(
                input,
                RecoveryCandidate::Restore {
                    input,
                    text,
                    reply_to,
                },
            );
        }
    }
    // The latest live projection wins over older history states.
    for input in &snapshot.inputs {
        candidates.remove(&input.id);
        if let Some(candidate) = recovery_candidate(input) {
            candidates.insert(input.id, candidate);
        }
    }
    candidates.into_values().rev().collect()
}

/// Explicit restore/conversion appends instead of overwriting an ordinary draft.
/// The caller increments that buffer's revision and moves its cursor to the end.
pub fn append_restored_text(existing: &str, restored: &str) -> String {
    if existing.is_empty() {
        return restored.to_owned();
    }
    if restored.is_empty() {
        return existing.to_owned();
    }
    format!(
        "{existing}{}{restored}",
        if existing.ends_with('\n') { "" } else { "\n" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use bone_app::{SessionId, SessionInfo, SessionSeq, WorkspaceId};

    fn question() -> QuestionId {
        QuestionId {
            runtime: RuntimeId::new(),
            record: 7,
            reply_to: InputId(1),
        }
    }
    fn input(id: u64, text: &str, state: InputState) -> InputView {
        InputView {
            id: InputId(id),
            request_id: RequestId::new(),
            text: text.into(),
            reply_to: None,
            state,
        }
    }
    fn snapshot(inputs: Vec<InputView>) -> SessionView {
        SessionView {
            session: SessionInfo {
                id: SessionId::new(),
                workspace: WorkspaceId::new(),
                title: "test".into(),
                archived: false,
            },
            runtime: RuntimeState::Detached,
            draft: String::new(),
            inputs,
            jobs: vec![],
            activity: vec![],
            history_through: SessionSeq(0),
            problem: None,
        }
    }
    fn entry(sequence: u64, event: SessionEvent) -> HistoryEntry {
        HistoryEntry {
            sequence: SessionSeq(sequence),
            occurred_at: 0,
            event,
        }
    }

    #[test]
    fn active_question_requires_full_identity_and_current_runtime() {
        let q = question();
        let inputs = vec![input(
            1,
            "request",
            InputState::WaitingForUser {
                runtime: q.runtime,
                question: q,
                text: "Scope?".into(),
            },
        )];
        assert_eq!(
            matching_question(&inputs, Some(q.runtime), q).unwrap().text,
            "Scope?"
        );
        assert!(matching_question(&inputs, Some(RuntimeId::new()), q).is_none());
        assert!(
            matching_question(&inputs, Some(q.runtime), QuestionId { record: 8, ..q }).is_none()
        );
        assert!(active_questions(&snapshot(inputs)).is_empty());
    }

    #[test]
    fn expired_answers_cannot_fall_back_to_ordinary_submissions() {
        let mut answer = AnswerDraft::new(question());
        answer.replace("/new is my answer".into(), usize::MAX);
        assert!(matches!(
            answer.submission(&snapshot(vec![]), RequestId::new()),
            Err(AnswerError::QuestionExpired)
        ));
        assert_eq!(answer.text, "/new is my answer");
    }

    #[test]
    fn answer_receipt_matches_question_text_and_revision() {
        let q = question();
        let mut answer = AnswerDraft::new(q);
        answer.replace("first".into(), 5);
        let old_revision = answer.revision;
        answer.replace("first plus later typing".into(), usize::MAX);
        assert!(!answer.clear_if_submitted(q, old_revision, "first"));
        assert!(!answer.clear_if_submitted(
            QuestionId { record: 8, ..q },
            answer.revision,
            "first plus later typing"
        ));
        assert!(answer.clear_if_submitted(q, answer.revision, "first plus later typing"));
        assert_eq!((answer.text.as_str(), answer.cursor), ("", 0));
    }

    #[test]
    fn replacing_answer_does_not_split_combining_clusters() {
        let mut answer = AnswerDraft::new(question());
        answer.replace("中e\u{301}文".into(), 4);
        assert_eq!(answer.cursor, "中".len());
        assert_eq!(
            append_restored_text("ordinary draft", &answer.text),
            "ordinary draft\n中e\u{301}文"
        );
        assert_eq!(answer.text, "中e\u{301}文");
    }

    #[test]
    fn retry_is_only_offered_for_supported_input_states() {
        let q = question();
        assert_eq!(
            recovery_candidate(&input(1, "text", InputState::Queued { problem: None })),
            Some(RecoveryCandidate::Retry { input: InputId(1) })
        );
        assert_eq!(
            recovery_candidate(&input(
                2,
                "text",
                InputState::RoutingFailed {
                    runtime: q.runtime,
                    message: "network".into()
                }
            )),
            Some(RecoveryCandidate::Retry { input: InputId(2) })
        );
        assert!(
            recovery_candidate(&input(
                3,
                "text",
                InputState::Accepted { runtime: q.runtime }
            ))
            .is_none()
        );
        let mut cancelled = input(4, "answer", InputState::Cancelled);
        cancelled.reply_to = Some(q);
        assert_eq!(
            recovery_candidate(&cancelled),
            Some(RecoveryCandidate::Restore {
                input: InputId(4),
                text: "answer".into(),
                reply_to: Some(q)
            })
        );
    }

    #[test]
    fn stopped_inputs_restore_from_history_without_losing_answer_binding() {
        let q = question();
        let events = vec![
            entry(
                1,
                SessionEvent::InputSubmitted {
                    input: InputId(2),
                    request_id: RequestId::new(),
                    text: "bound answer".into(),
                    reply_to: Some(q),
                },
            ),
            entry(2, SessionEvent::InputCancelled { input: InputId(2) }),
        ];
        assert_eq!(
            recoverable_inputs(&snapshot(vec![]), &events),
            vec![RecoveryCandidate::Restore {
                input: InputId(2),
                text: "bound answer".into(),
                reply_to: Some(q)
            }]
        );
        assert!(recoverable_inputs(&snapshot(vec![]), &events[1..]).is_empty());
        let current = snapshot(vec![input(
            2,
            "bound answer",
            InputState::Accepted { runtime: q.runtime },
        )]);
        assert!(recoverable_inputs(&current, &events).is_empty());
    }

    #[test]
    fn submitted_history_alone_never_invents_retry_and_restore_preserves_draft() {
        let events = vec![entry(
            1,
            SessionEvent::InputSubmitted {
                input: InputId(1),
                request_id: RequestId::new(),
                text: "maybe already finished".into(),
                reply_to: None,
            },
        )];
        assert!(recoverable_inputs(&snapshot(vec![]), &events).is_empty());
        assert_eq!(
            append_restored_text("new draft\n", "cancelled text"),
            "new draft\ncancelled text"
        );
    }
}
