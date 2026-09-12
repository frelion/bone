use crate::{
    editor::{CursorMove, EditCommand},
    state::{
        Action, EditorTarget, Effect, Focus, SessionNavRow, SessionUi, UiEvent, UiState, update,
    },
};
use bone_app::{
    HistoryEntry, InputId, InputState, InputView, ModelSelection, Profile, ProfileId, QuestionId,
    RecentHistoryPage, RequestId, ResolvedModel, RuntimeConfig, RuntimeId, RuntimeState,
    SessionEvent, SessionId, SessionInfo, SessionSeq, SessionView, WorkspaceId,
};
use std::sync::Arc;

fn info(name: &str) -> SessionInfo {
    SessionInfo {
        id: SessionId::new(),
        workspace: WorkspaceId::new(),
        title: name.into(),
        archived: false,
    }
}
fn view(info: &SessionInfo, q: Option<QuestionId>) -> SessionView {
    let model = ResolvedModel {
        selection: ModelSelection::new(ProfileId::chatgpt(), "test").unwrap(),
        profile: Profile::chatgpt(),
    };
    SessionView {
        session: info.clone(),
        runtime: q.map_or(RuntimeState::Detached, |q| RuntimeState::Running {
            id: q.runtime,
            config: Box::new(RuntimeConfig {
                coordinator: model.clone(),
                worker: model,
                limits: Default::default(),
                tools: Default::default(),
                workspace: Default::default(),
            }),
        }),
        draft: String::new(),
        inputs: q
            .into_iter()
            .map(|q| InputView {
                id: q.reply_to,
                request_id: RequestId::new(),
                text: "original request".into(),
                reply_to: None,
                state: InputState::WaitingForUser {
                    runtime: q.runtime,
                    question: q,
                    text: "Which scope?".into(),
                },
            })
            .collect(),
        jobs: vec![],
        activity: vec![],
        history_through: SessionSeq(0),
        problem: None,
    }
}
fn setup() -> (UiState, SessionId, QuestionId) {
    let info = info("answer test");
    let q = QuestionId {
        runtime: RuntimeId::new(),
        record: 10,
        reply_to: InputId(1),
    };
    let mut ui = SessionUi::new(info.id, 1);
    ui.snapshot = Some(Arc::new(view(&info, Some(q))));
    ui.hydrated = true;
    let mut state = UiState::default();
    state.selected = Some(info.id);
    state.session_rows = vec![SessionNavRow::provisional(info.clone())];
    state.session_ui.insert(info.id, ui);
    (state, info.id, q)
}
fn act(s: &mut UiState, action: Action) -> Vec<Effect> {
    update(s, UiEvent::Action(action))
}
fn edit(command: EditCommand) -> Action {
    Action::Edit {
        target: EditorTarget::Composer,
        command,
    }
}
fn insert(text: impl Into<String>) -> Action {
    edit(EditCommand::Insert {
        text: text.into(),
        typing: false,
    })
}
fn submitted(effects: &[Effect]) -> bone_app::SubmitInput {
    effects
        .iter()
        .find_map(|e| {
            if let Effect::Submit { input, .. } = e {
                Some(input.clone())
            } else {
                None
            }
        })
        .expect("submit effect")
}
fn receipt(s: &mut UiState, session: SessionId, request_id: RequestId) -> Vec<Effect> {
    update(
        s,
        UiEvent::Submitted {
            session,
            request_id,
        },
    )
}

#[test]
fn answer_buffer_is_separate_and_slash_answers_stay_answers() {
    let (mut s, id, q) = setup();
    act(&mut s, insert("ordinary draft"));
    act(&mut s, Action::AnswerQuestion(q));
    act(&mut s, insert("/new is an answer, not a command"));
    assert!(s.slash_matches().is_empty());
    let input = submitted(&act(&mut s, Action::Submit));
    assert_eq!(input.reply_to, Some(q));
    assert_eq!(input.text, "/new is an answer, not a command");
    let effects = receipt(&mut s, id, input.request_id);
    assert_eq!(s.draft(), "ordinary draft");
    assert!(
        effects
            .iter()
            .all(|e| !matches!(e, Effect::SaveDraft { .. } | Effect::AutoTitle { .. }))
    );
}

#[test]
fn answer_receipt_survives_reopen_but_does_not_clear_new_answer_text() {
    let (mut s, id, q) = setup();
    act(&mut s, Action::AnswerQuestion(q));
    act(&mut s, insert("first"));
    let input = submitted(&act(&mut s, Action::Submit));
    act(&mut s, insert(" later"));
    s.session_ui.get_mut(&id).unwrap().generation = 99;
    receipt(&mut s, id, input.request_id);
    assert_eq!(s.draft(), "first later");
    assert!(s.selected_ui().unwrap().submitting.is_none());
    assert_eq!(s.selected_ui().unwrap().draft(), "");
}

#[test]
fn escape_restores_ordinary_draft_and_expiry_requires_explicit_conversion() {
    let (mut s, id, q) = setup();
    act(&mut s, insert("ordinary"));
    act(&mut s, Action::AnswerQuestion(q));
    act(&mut s, insert("answer"));
    assert!(act(&mut s, Action::Escape).is_empty());
    assert_eq!(s.draft(), "ordinary");
    act(&mut s, Action::AnswerQuestion(q));
    let info = s.session_row(id).unwrap().info().clone();
    s.session_ui.get_mut(&id).unwrap().snapshot = Some(Arc::new(view(&info, None)));
    assert!(act(&mut s, Action::Submit).is_empty());
    assert_eq!(s.draft(), "answer");
    act(&mut s, Action::ConvertAnswer);
    assert_eq!(s.draft(), "ordinary\nanswer");
    assert_eq!(submitted(&act(&mut s, Action::Submit)).reply_to, None);
}

#[test]
fn answer_editing_and_persistence_do_not_touch_the_ordinary_buffer() {
    let (mut s, _, q) = setup();
    act(&mut s, insert("ordinary"));
    act(&mut s, Action::AnswerQuestion(q));
    act(&mut s, insert("中e\u{301}\nnext"));
    act(
        &mut s,
        edit(EditCommand::Move {
            cursor: CursorMove::LineStart,
            select: false,
        }),
    );
    act(
        &mut s,
        edit(EditCommand::Move {
            cursor: CursorMove::Up { width: 30 },
            select: false,
        }),
    );
    act(&mut s, edit(EditCommand::DeleteAfter));
    assert_eq!(s.draft(), "e\u{301}\nnext");
    let saves = update(&mut s, UiEvent::PersistDraftsRequested);
    assert!(
        saves
            .iter()
            .any(|e| matches!(e, Effect::SaveDraft { text, .. } if text == "ordinary"))
    );
    assert!(
        saves
            .iter()
            .all(|e| !matches!(e, Effect::SaveDraft { text, .. } if text.contains("next")))
    );
}

#[test]
fn saved_input_retry_and_cancelled_answer_restore_keep_identity() {
    let (mut s, id, q) = setup();
    let mut snapshot = s
        .selected_ui()
        .unwrap()
        .snapshot
        .as_ref()
        .unwrap()
        .as_ref()
        .clone();
    snapshot.inputs.push(InputView {
        id: InputId(3),
        request_id: RequestId::new(),
        text: "retry me".into(),
        reply_to: None,
        state: InputState::Queued { problem: None },
    });
    s.session_ui.get_mut(&id).unwrap().snapshot = Some(Arc::new(snapshot));
    assert!(matches!(
        act(&mut s, Action::RetryInput(InputId(3))).as_slice(),
        [Effect::RetryInput {
            input: InputId(3),
            ..
        }]
    ));
    let ui = s.session_ui.get_mut(&id).unwrap();
    ui.transcript.open(RecentHistoryPage {
        items: vec![
            HistoryEntry {
                sequence: SessionSeq(1),
                occurred_at: 0,
                event: SessionEvent::InputSubmitted {
                    input: InputId(4),
                    request_id: RequestId::new(),
                    text: "cancelled answer".into(),
                    reply_to: Some(q),
                },
            },
            HistoryEntry {
                sequence: SessionSeq(2),
                occurred_at: 0,
                event: SessionEvent::InputCancelled { input: InputId(4) },
            },
        ],
        older_cursor: None,
        snapshot_through: SessionSeq(2),
    });
    act(&mut s, Action::RestoreInput(InputId(4)));
    assert_eq!(s.draft(), "cancelled answer");
    assert_eq!(s.selected_ui().unwrap().selected_answer, Some(q));
    assert!(s.selected_ui().unwrap().draft().is_empty());
}

#[test]
fn uncertain_answer_retry_reuses_request_and_preserves_later_text() {
    let (mut s, id, q) = setup();
    act(&mut s, Action::AnswerQuestion(q));
    act(&mut s, insert("first"));
    let original = submitted(&act(&mut s, Action::Submit));
    act(&mut s, insert(" later"));
    update(
        &mut s,
        UiEvent::SubmitFailed {
            session: id,
            request_id: original.request_id,
            message: "uncertain".into(),
        },
    );
    assert!(act(&mut s, Action::Submit).is_empty());
    let retry = submitted(&act(&mut s, Action::RetrySubmission));
    assert_eq!(
        (retry.request_id, retry.reply_to, retry.text.as_str()),
        (original.request_id, Some(q), "first")
    );
    receipt(&mut s, id, retry.request_id);
    assert_eq!(s.draft(), "first later");
}

#[test]
fn first_input_creates_then_submits_after_hydration_and_preserves_later_typing() {
    let mut s = UiState::default();
    act(&mut s, insert("first request"));
    let effects = act(&mut s, Action::Submit);
    let create_id = effects
        .iter()
        .find_map(|e| {
            if let Effect::CreateSession { request_id, .. } = e {
                Some(*request_id)
            } else {
                None
            }
        })
        .unwrap();
    assert!(act(&mut s, Action::Submit).is_empty());
    act(&mut s, insert(" and later draft"));
    let info = info("new session");
    let effects = update(
        &mut s,
        UiEvent::SessionCreated {
            request_id: create_id,
            info: info.clone(),
        },
    );
    assert!(effects.iter().all(|e| !matches!(e, Effect::Submit { .. })));
    let generation = s.selected_ui().unwrap().generation;
    let effects = update(
        &mut s,
        UiEvent::SessionOpened {
            session: info.id,
            generation,
            snapshot: Arc::new(view(&info, None)),
            history: RecentHistoryPage {
                items: vec![],
                older_cursor: None,
                snapshot_through: SessionSeq(0),
            },
        },
    );
    let input = submitted(&effects);
    assert_eq!(input.text, "first request");
    receipt(&mut s, info.id, input.request_id);
    assert_eq!(s.draft(), "first request and later draft");
    s.selected = None;
    assert!(s.draft().is_empty());
}

#[test]
fn first_input_create_failure_reuses_identity_and_keeps_text() {
    let mut s = UiState::default();
    act(&mut s, insert("first request"));
    act(&mut s, Action::Submit);
    let request_id = s.pending_create.as_ref().unwrap().request_id;
    update(
        &mut s,
        UiEvent::SessionCreateFailed {
            request_id,
            message: "uncertain".into(),
        },
    );
    act(&mut s, insert(" later"));
    let retry = act(&mut s, Action::Submit);
    assert!(
        matches!(retry.as_slice(), [Effect::CreateSession { request_id: actual, .. }] if *actual == request_id)
    );
    assert_eq!(s.draft(), "first request later");
    assert_eq!(s.focus, Focus::Composer);
}

#[test]
fn quitting_preserves_answer_and_ordinary_draft_once_without_submitting() {
    let (mut state, id, question) = setup();
    act(&mut state, insert("ordinary draft"));
    let ui = state.session_ui.get_mut(&id).unwrap();
    let mut answer = crate::state::answer::AnswerDraft::new(question);
    answer.replace("answer text".into(), 11);
    ui.answer_drafts.insert(question, answer);
    for _ in 0..2 {
        let effects = act(&mut state, Action::Quit);
        assert!(effects.iter().any(|e| matches!(e, Effect::Shutdown)));
        assert!(!effects.iter().any(|e| matches!(e, Effect::Submit { .. })));
    }
    let draft = state.session_ui[&id].draft();
    assert!(draft.starts_with("ordinary draft"));
    assert!(draft.contains("未发送回答"));
    assert_eq!(draft.matches("answer text").count(), 1);
    assert!(state.session_ui[&id].answer_drafts.is_empty());
}

#[test]
fn converted_answer_is_not_copied_again_on_quit() {
    let (mut state, id, question) = setup();
    let ui = state.session_ui.get_mut(&id).unwrap();
    let mut answer = crate::state::answer::AnswerDraft::new(question);
    answer.replace("converted answer".into(), 16);
    ui.answer_drafts.insert(question, answer);
    ui.selected_answer = Some(question);
    act(&mut state, Action::ConvertAnswer);
    act(&mut state, Action::Quit);
    assert_eq!(state.session_ui[&id].draft(), "converted answer");
    assert!(state.session_ui[&id].answer_drafts.is_empty());
}
