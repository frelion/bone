use std::{collections::BTreeMap, sync::Arc};

use bone_app::{
    AttentionItem, HistoryEntry, HistoryPage, QuestionId, RecentHistoryPage, SessionId,
    SessionInfo, SessionView, SubmissionReceipt, WorkspaceId,
};

use super::*;

fn session(title: &str) -> SessionInfo {
    SessionInfo {
        id: SessionId::new(),
        workspace: WorkspaceId::new(),
        title: title.into(),
        archived: false,
    }
}

fn result(info: &SessionInfo) -> bone_app::ResultSummary {
    bone_app::ResultSummary {
        result: bone_app::ResultRef {
            session: info.id,
            job: bone_app::JobRef {
                runtime: bone_app::RuntimeId::new(),
                id: 7,
            },
            version: bone_app::SessionSeq(11),
        },
        outcome: bone_app::OutcomeKind::Completed,
        summary: "完成了工作".into(),
        remaining: Vec::new(),
    }
}

fn begin_acceptance(state: &mut UiState, decision: bone_app::AcceptanceDecision) -> Vec<Effect> {
    let result = state
        .selected
        .and_then(|session| state.results.get(&session))
        .and_then(|page| page.items.last())
        .expect("test result")
        .result;
    update(
        state,
        UiEvent::Action(Action::BeginAcceptance { decision, result }),
    )
}

fn mark_selected_open(state: &mut UiState) {
    let ui = state.selected_ui_mut().expect("selected session");
    ui.snapshot = Some(Arc::new(SessionView {
        session: ui.info.clone(),
        runtime: bone_app::RuntimeState::Detached,
        draft: ui.draft.clone(),
        inputs: Vec::new(),
        jobs: Vec::new(),
        activity: Vec::new(),
        history_through: bone_app::SessionSeq(0),
        problem: None,
    }));
}

fn snapshot(info: &SessionInfo, draft: &str) -> Arc<SessionView> {
    Arc::new(SessionView {
        session: info.clone(),
        runtime: bone_app::RuntimeState::Detached,
        draft: draft.into(),
        inputs: Vec::new(),
        jobs: Vec::new(),
        activity: Vec::new(),
        history_through: bone_app::SessionSeq(0),
        problem: None,
    })
}

fn history(sequence: u64) -> HistoryEntry {
    HistoryEntry {
        sequence: bone_app::SessionSeq(sequence),
        occurred_at: sequence as i64,
        event: bone_app::SessionEvent::InputCancelled {
            input: bone_app::InputId(sequence),
        },
    }
}

#[test]
fn global_and_detail_controls_are_reachable_with_focus_arrows_and_activate() {
    let info = session("navigation");
    let mut state = UiState {
        sessions: vec![info.clone()],
        ..UiState::default()
    };
    update(&mut state, UiEvent::SessionCreated(info));

    state.focus = Focus::Composer;
    for _ in 0..3 {
        update(&mut state, UiEvent::Action(Action::FocusNext));
    }
    assert_eq!(state.focus, Focus::Global);
    for expected in [MainView::Sessions, MainView::Attention, MainView::Settings] {
        state.focus = Focus::Global;
        update(&mut state, UiEvent::Action(Action::MoveFocusedControl(1)));
        update(&mut state, UiEvent::Action(Action::Activate));
        assert_eq!(state.main, expected);
    }

    state.main = MainView::Workbench;
    update(
        &mut state,
        UiEvent::Action(Action::OpenDetail(DetailState {
            kind: DetailKind::Work,
            title: "工作详情".into(),
        })),
    );
    for expected in [
        DetailKind::Changes,
        DetailKind::Context,
        DetailKind::Artifacts,
        DetailKind::Records,
        DetailKind::Acceptance,
    ] {
        state.focus = Focus::Detail;
        update(&mut state, UiEvent::Action(Action::MoveFocusedControl(1)));
        update(&mut state, UiEvent::Action(Action::Activate));
        assert_eq!(state.detail.as_ref().unwrap().kind, expected);
    }
}

#[test]
fn action_bar_controls_activate_the_same_product_actions_without_shortcuts() {
    let info = session("actions");
    let mut state = UiState {
        sessions: vec![info.clone()],
        ..UiState::default()
    };
    update(&mut state, UiEvent::SessionCreated(info.clone()));
    mark_selected_open(&mut state);
    state.selected_ui_mut().unwrap().draft = "send from action bar".into();
    state.focus = Focus::Composer;
    update(&mut state, UiEvent::Action(Action::FocusNext));
    update(&mut state, UiEvent::Action(Action::FocusNext));
    assert_eq!(state.focus, Focus::Actions);

    update(&mut state, UiEvent::Action(Action::Activate));
    assert_eq!(state.detail.as_ref().unwrap().kind, DetailKind::Work);
    state.detail = None;
    state.focus = Focus::Actions;
    state.action_selection = 1;
    let effects = update(&mut state, UiEvent::Action(Action::Activate));
    assert!(matches!(&effects[..], [Effect::Submit { session, .. }] if *session == info.id));

    state.focus = Focus::Actions;
    state.action_selection = 2;
    let effects = update(&mut state, UiEvent::Action(Action::Activate));
    assert!(matches!(&effects[..], [Effect::Stop { session, .. }] if *session == info.id));

    state.focus = Focus::Actions;
    state.action_selection = 3;
    update(&mut state, UiEvent::Action(Action::Activate));
    assert!(matches!(state.dialog, Some(Dialog::ConfirmQuit)));
}

#[test]
fn acceptance_is_disabled_until_the_exact_results_refresh_completes() {
    let info = session("acceptance refresh");
    let summary = result(&info);
    let page = bone_app::ResultPage {
        items: vec![summary.clone()],
        older_cursor: None,
        snapshot_through: bone_app::SessionSeq(11),
        projection_pending: false,
    };
    let mut state = UiState {
        sessions: vec![info.clone()],
        ..UiState::default()
    };
    update(&mut state, UiEvent::SessionCreated(info.clone()));
    state.results.insert(info.id, page.clone());
    let effects = update(
        &mut state,
        UiEvent::Action(Action::OpenDetail(DetailState {
            kind: DetailKind::Acceptance,
            title: "结果与验收".into(),
        })),
    );
    let Effect::LoadResults {
        generation, query, ..
    } = effects[0]
    else {
        panic!("expected authoritative result refresh")
    };
    assert!(state.acceptance_target().is_none());
    update(
        &mut state,
        UiEvent::Action(Action::BeginAcceptance {
            decision: bone_app::AcceptanceDecision::Accepted,
            result: summary.result,
        }),
    );
    assert!(state.dialog.is_none());
    update(
        &mut state,
        UiEvent::OperationFailed {
            kind: OperationKind::LoadResults,
            session: Some(info.id),
            generation: Some(generation),
            message: "refresh unavailable".into(),
        },
    );
    assert!(state.acceptance_target().is_none());

    update(
        &mut state,
        UiEvent::ResultsLoaded {
            session: info.id,
            generation,
            query,
            page,
        },
    );
    assert_eq!(state.acceptance_target(), Some(summary.result));
    update(
        &mut state,
        UiEvent::Action(Action::BeginAcceptance {
            decision: bone_app::AcceptanceDecision::Accepted,
            result: summary.result,
        }),
    );
    assert!(matches!(
        state.dialog,
        Some(Dialog::Acceptance { result, .. }) if result == summary.result
    ));
}

#[test]
fn changed_result_snapshot_releases_the_old_request_before_reloading() {
    let info = session("result snapshot changed");
    let summary = result(&info);
    let mut state = UiState {
        sessions: vec![info.clone()],
        ..UiState::default()
    };
    update(&mut state, UiEvent::SessionCreated(info.clone()));

    let first = update(
        &mut state,
        UiEvent::Action(Action::OpenDetail(DetailState {
            kind: DetailKind::Acceptance,
            title: "结果与验收".into(),
        })),
    );
    let Effect::LoadResults {
        generation, query, ..
    } = first[0]
    else {
        panic!("expected initial results request")
    };
    update(
        &mut state,
        UiEvent::ResultsLoaded {
            session: info.id,
            generation,
            query,
            page: bone_app::ResultPage {
                items: vec![summary.clone()],
                older_cursor: None,
                snapshot_through: bone_app::SessionSeq(11),
                projection_pending: false,
            },
        },
    );

    let refresh = update(
        &mut state,
        UiEvent::Action(Action::OpenDetail(DetailState {
            kind: DetailKind::Acceptance,
            title: "结果与验收".into(),
        })),
    );
    let Effect::LoadResults {
        generation, query, ..
    } = refresh[0]
    else {
        panic!("expected refresh request")
    };
    let effects = update(
        &mut state,
        UiEvent::ResultsLoaded {
            session: info.id,
            generation,
            query,
            page: bone_app::ResultPage {
                items: vec![summary],
                older_cursor: None,
                snapshot_through: bone_app::SessionSeq(12),
                projection_pending: false,
            },
        },
    );

    assert!(matches!(effects.as_slice(), [Effect::LoadResults { .. }]));
    assert!(state.session_ui[&info.id].result_loading);
    assert!(state.session_ui[&info.id].result_pages.pending.is_some());
}

#[test]
fn switching_to_a_session_without_detail_closes_it_and_rejects_late_artifact_data() {
    let first = session("first");
    let second = session("second");
    let result = result(&first).result;
    let mut state = UiState {
        sessions: vec![first.clone(), second.clone()],
        ..UiState::default()
    };
    update(&mut state, UiEvent::SessionCreated(first.clone()));
    state.detail = Some(DetailState {
        kind: DetailKind::Artifacts,
        title: "产物与证据".into(),
    });
    state.artifact.query = 17;
    update(
        &mut state,
        UiEvent::Action(Action::SelectSession(second.id)),
    );
    assert!(state.detail.is_none());
    assert_ne!(state.selected, Some(first.id));

    update(
        &mut state,
        UiEvent::ArtifactLoaded {
            result,
            query: 17,
            artifact: bone_app::ResultArtifact {
                result,
                outcome: bone_app::OutcomeKind::Completed,
                summary: "late".into(),
                remaining: Vec::new(),
                evidence_count: 0,
            },
            evidence: bone_app::EvidencePage {
                result,
                items: Vec::new(),
                next_cursor: None,
                projection_pending: false,
            },
        },
    );
    assert!(state.artifact.artifact.is_none());
}

#[test]
fn switching_back_restores_that_sessions_detail_and_reloads_app_data() {
    let first = session("first detail");
    let second = session("second detail");
    let mut state = UiState {
        sessions: vec![first.clone(), second.clone()],
        ..UiState::default()
    };
    update(&mut state, UiEvent::SessionCreated(first.clone()));
    update(&mut state, UiEvent::SessionCreated(second.clone()));
    update(&mut state, UiEvent::Action(Action::SelectSession(first.id)));
    let original_effects = update(
        &mut state,
        UiEvent::Action(Action::OpenDetail(DetailState {
            kind: DetailKind::Artifacts,
            title: "产物与证据".into(),
        })),
    );
    let original_query = original_effects
        .iter()
        .find_map(|effect| match effect {
            Effect::LoadResults { query, .. } => Some(*query),
            _ => None,
        })
        .expect("initial artifact result query");
    state.detail_scroll = 9;

    update(
        &mut state,
        UiEvent::Action(Action::SelectSession(second.id)),
    );
    assert!(state.detail.is_none());
    let effects = update(&mut state, UiEvent::Action(Action::SelectSession(first.id)));
    assert_eq!(state.detail.as_ref().unwrap().kind, DetailKind::Artifacts);
    assert_eq!(state.detail_scroll, 9);
    let restored_query = effects
        .iter()
        .find_map(|effect| match effect {
            Effect::LoadResults { session, query, .. } if *session == first.id => Some(*query),
            _ => None,
        })
        .unwrap_or_else(|| panic!("restored artifact effects: {effects:?}"));
    assert_ne!(restored_query, original_query);
    let generation = state.session_ui.get(&first.id).unwrap().generation;
    update(
        &mut state,
        UiEvent::ResultsLoaded {
            session: first.id,
            generation,
            query: original_query,
            page: bone_app::ResultPage {
                items: vec![result(&first)],
                older_cursor: None,
                snapshot_through: bone_app::SessionSeq(11),
                projection_pending: false,
            },
        },
    );
    assert!(!state.results.contains_key(&first.id));
}

#[test]
fn selecting_the_current_session_does_not_replace_its_live_detail_state() {
    let info = session("current");
    let mut state = UiState {
        sessions: vec![info.clone()],
        ..UiState::default()
    };
    update(&mut state, UiEvent::SessionCreated(info.clone()));
    state.detail = Some(DetailState {
        kind: DetailKind::Context,
        title: "会话上下文".into(),
    });
    state.detail_scroll = 13;
    state.session_ui.get_mut(&info.id).unwrap().saved_detail = Some(DetailState {
        kind: DetailKind::Work,
        title: "旧详情".into(),
    });
    update(&mut state, UiEvent::Action(Action::SelectSession(info.id)));
    assert_eq!(state.detail.as_ref().unwrap().kind, DetailKind::Context);
    assert_eq!(state.detail_scroll, 13);
}

#[test]
fn late_open_does_not_update_a_new_generation() {
    let info = session("one");
    let mut state = UiState {
        sessions: vec![info.clone()],
        ..UiState::default()
    };
    let effects = update(&mut state, UiEvent::SessionCreated(info.clone()));
    let Effect::OpenSession { generation, .. } = effects[0] else {
        panic!("expected open")
    };
    state.session_ui.get_mut(&info.id).unwrap().generation += 1;
    update(
        &mut state,
        UiEvent::OperationFailed {
            kind: OperationKind::OpenSession,
            session: Some(info.id),
            generation: Some(generation),
            message: "late".into(),
        },
    );
    assert!(state.status.is_none());
}

#[test]
fn submit_receipt_does_not_clear_text_typed_after_submit() {
    let info = session("one");
    let mut state = UiState {
        sessions: vec![info.clone()],
        ..UiState::default()
    };
    update(&mut state, UiEvent::SessionCreated(info.clone()));
    mark_selected_open(&mut state);
    for value in "hello".chars() {
        update(&mut state, UiEvent::Action(Action::Input(value)));
    }
    let effects = update(&mut state, UiEvent::Action(Action::Submit));
    let Effect::Submit {
        generation, input, ..
    } = effects[0].clone()
    else {
        panic!("expected submit")
    };
    update(&mut state, UiEvent::Action(Action::Input('!')));
    update(
        &mut state,
        UiEvent::Submitted {
            session: info.id,
            generation,
            request_id: input.request_id,
            receipt: SubmissionReceipt {
                input: bone_app::InputId(1),
                saved_at: bone_app::SessionSeq(1),
            },
        },
    );
    assert_eq!(state.selected_ui().unwrap().draft, "!");
}

#[test]
fn failed_submit_retries_the_same_request_without_losing_follow_up() {
    let info = session("one");
    let mut state = UiState {
        sessions: vec![info.clone()],
        ..UiState::default()
    };
    update(&mut state, UiEvent::SessionCreated(info.clone()));
    mark_selected_open(&mut state);
    update(&mut state, UiEvent::Action(Action::Input('A')));
    let first = update(&mut state, UiEvent::Action(Action::Submit));
    let Effect::Submit {
        generation, input, ..
    } = first[0].clone()
    else {
        panic!("expected submit")
    };
    update(&mut state, UiEvent::Action(Action::Input('B')));
    update(
        &mut state,
        UiEvent::OperationFailed {
            kind: OperationKind::Submit,
            session: Some(info.id),
            generation: Some(generation),
            message: "reply lost".into(),
        },
    );
    let retry = update(&mut state, UiEvent::Action(Action::Submit));
    let Effect::Submit { input: retried, .. } = &retry[0] else {
        panic!("expected retry")
    };
    assert_eq!(retried.request_id, input.request_id);
    assert_eq!(retried.text, "A");
    assert_eq!(state.selected_ui().unwrap().draft, "AB");
}

#[test]
fn escape_closes_detail_without_stopping_work() {
    let mut state = UiState {
        detail: Some(DetailState {
            kind: DetailKind::Work,
            title: "task".into(),
        }),
        ..UiState::default()
    };
    let effects = update(&mut state, UiEvent::Action(Action::Escape));
    assert!(state.detail.is_none());
    assert!(effects.is_empty());
}

#[test]
fn backspace_removes_one_user_perceived_character() {
    let info = session("one");
    let mut state = UiState {
        sessions: vec![info.clone()],
        ..UiState::default()
    };
    update(&mut state, UiEvent::SessionCreated(info));
    update(
        &mut state,
        UiEvent::Action(Action::Paste("e\u{301}".into())),
    );
    update(&mut state, UiEvent::Action(Action::Backspace));
    assert_eq!(state.selected_ui().unwrap().draft, "");
    update(&mut state, UiEvent::Action(Action::Paste("👨‍👩‍👧‍👦".into())));
    update(&mut state, UiEvent::Action(Action::Backspace));
    assert_eq!(state.selected_ui().unwrap().draft, "");
}

#[test]
fn submit_is_scoped_to_the_focused_composer() {
    let info = session("one");
    let mut state = UiState {
        sessions: vec![info.clone()],
        ..UiState::default()
    };
    update(&mut state, UiEvent::SessionCreated(info));
    update(&mut state, UiEvent::Action(Action::Input('A')));
    state.main = MainView::Settings;
    assert!(update(&mut state, UiEvent::Action(Action::Submit)).is_empty());
    state.main = MainView::Workbench;
    state.focus = Focus::Timeline;
    assert!(update(&mut state, UiEvent::Action(Action::Submit)).is_empty());
    assert_eq!(state.selected_ui().unwrap().draft, "A");
}

#[test]
fn idle_tick_does_not_request_a_redraw() {
    let mut state = UiState {
        dirty: false,
        ..UiState::default()
    };
    assert!(update(&mut state, UiEvent::Tick).is_empty());
    assert!(!state.dirty);
}

#[test]
fn rejected_acceptance_requires_reason_and_a_fresh_rework_request() {
    let info = session("one");
    let mut state = UiState {
        sessions: vec![info.clone()],
        ..UiState::default()
    };
    update(&mut state, UiEvent::SessionCreated(info.clone()));
    state.results.insert(
        info.id,
        bone_app::ResultPage {
            items: vec![result(&info)],
            older_cursor: None,
            snapshot_through: bone_app::SessionSeq(11),
            projection_pending: false,
        },
    );

    begin_acceptance(&mut state, bone_app::AcceptanceDecision::Rejected);
    assert!(update(&mut state, UiEvent::Action(Action::Submit)).is_empty());
    update(
        &mut state,
        UiEvent::Action(Action::Paste("没有覆盖边界".into())),
    );
    assert!(update(&mut state, UiEvent::Action(Action::Submit)).is_empty());
    update(&mut state, UiEvent::Action(Action::FocusNext));
    update(
        &mut state,
        UiEvent::Action(Action::Paste("补齐边界测试后重新提交".into())),
    );
    let effects = update(&mut state, UiEvent::Action(Action::Submit));
    let Effect::SubmitAcceptance { submission, .. } = &effects[0] else {
        panic!("expected acceptance submission")
    };
    assert_eq!(submission.decision, bone_app::AcceptanceDecision::Rejected);
    assert_eq!(submission.reason, "没有覆盖边界");
    let rework = submission.rework.as_ref().expect("new rework input");
    assert_eq!(rework.text, "补齐边界测试后重新提交");
    assert!(rework.reply_to.is_none());
    let original = submission.clone();
    update(
        &mut state,
        UiEvent::AcceptanceFailed {
            session: info.id,
            generation: 1,
            request_id: original.request_id,
            message: "receipt lost".into(),
        },
    );
    let retry = update(&mut state, UiEvent::Action(Action::Submit));
    let Effect::SubmitAcceptance {
        submission: retried,
        ..
    } = &retry[0]
    else {
        panic!("expected rejected acceptance retry")
    };
    assert_eq!(retried, &original);
}

#[test]
fn acceptance_retry_keeps_the_same_idempotency_key() {
    let info = session("one");
    let mut state = UiState {
        sessions: vec![info.clone()],
        ..UiState::default()
    };
    update(&mut state, UiEvent::SessionCreated(info.clone()));
    state.results.insert(
        info.id,
        bone_app::ResultPage {
            items: vec![result(&info)],
            older_cursor: None,
            snapshot_through: bone_app::SessionSeq(11),
            projection_pending: false,
        },
    );
    begin_acceptance(&mut state, bone_app::AcceptanceDecision::Rejected);
    if let Some(Dialog::Acceptance { reason, rework, .. }) = &mut state.dialog {
        *reason = "边界不完整".into();
        *rework = "补齐验证".into();
    }
    let first = update(&mut state, UiEvent::Action(Action::Submit));
    let Effect::SubmitAcceptance {
        generation,
        submission,
        ..
    } = first[0].clone()
    else {
        panic!("expected acceptance submission")
    };
    update(
        &mut state,
        UiEvent::AcceptanceFailed {
            session: info.id,
            generation,
            request_id: submission.request_id,
            message: "receipt lost".into(),
        },
    );
    let retry = update(&mut state, UiEvent::Action(Action::Submit));
    let Effect::SubmitAcceptance {
        submission: retried,
        ..
    } = &retry[0]
    else {
        panic!("expected acceptance retry")
    };
    assert_eq!(retried.request_id, submission.request_id);
    assert_eq!(retried.result, submission.result);
}

#[test]
fn older_page_survives_cache_eviction_instead_of_advancing_past_it() {
    let info = session("history");
    let mut ui = SessionUi::new(info.clone(), 1);
    for sequence in 513..=1024 {
        let entry = history(sequence);
        ui.history_bytes += history_entry_bytes(&entry);
        ui.history.push_back(entry);
    }
    ui.scroll_from_tail = 511;
    let mut state = UiState {
        sessions: vec![info.clone()],
        selected: Some(info.id),
        ..UiState::default()
    };
    state.session_ui.insert(info.id, ui);
    update(
        &mut state,
        UiEvent::OlderHistoryLoaded {
            session: info.id,
            generation: 1,
            page: RecentHistoryPage {
                items: (481..=512).map(history).collect(),
                older_cursor: None,
                snapshot_through: bone_app::SessionSeq(1024),
            },
        },
    );
    let ui = state.selected_ui().unwrap();
    assert_eq!(
        ui.history.front().unwrap().sequence,
        bone_app::SessionSeq(481)
    );
    assert_eq!(ui.history.len(), HISTORY_CACHE_ITEMS);
    assert!(ui.newer_history_missing);
}

#[test]
fn appended_history_keeps_the_current_reading_anchor() {
    let info = session("history");
    let mut ui = SessionUi::new(info.clone(), 1);
    ui.history.extend((1..=50).map(history));
    ui.history_cursor = bone_app::SessionSeq(50);
    ui.history_has_more = true;
    ui.scroll_from_tail = 20;
    let mut state = UiState {
        sessions: vec![info.clone()],
        selected: Some(info.id),
        ..UiState::default()
    };
    state.session_ui.insert(info.id, ui);
    update(
        &mut state,
        UiEvent::HistoryLoaded {
            session: info.id,
            generation: 1,
            page: HistoryPage {
                items: vec![history(51)],
                next_cursor: bone_app::SessionSeq(51),
                has_more: false,
            },
        },
    );
    assert_eq!(state.selected_ui().unwrap().scroll_from_tail, 21);
}

#[test]
fn typing_while_session_opens_is_preserved_and_saved_after_hydration() {
    let info = session("slow");
    let mut state = UiState {
        sessions: vec![info.clone()],
        ..UiState::default()
    };
    update(&mut state, UiEvent::SessionCreated(info.clone()));
    update(&mut state, UiEvent::Action(Action::Input('A')));
    assert!(update(&mut state, UiEvent::Action(Action::Submit)).is_empty());
    assert_eq!(state.selected_ui().unwrap().draft, "A");
    let effects = update(
        &mut state,
        UiEvent::SessionOpened {
            session: info.id,
            generation: 1,
            snapshot: snapshot(&info, "old"),
            history: RecentHistoryPage {
                items: Vec::new(),
                older_cursor: None,
                snapshot_through: bone_app::SessionSeq(0),
            },
        },
    );
    assert_eq!(state.selected_ui().unwrap().draft, "old\nA");
    assert!(effects.iter().any(|effect| matches!(
        effect,
        Effect::SaveDraft { text, .. } if text == "old\nA"
    )));
}

#[test]
fn submitting_acceptance_cannot_be_dismissed_or_replaced() {
    let info = session("one");
    let mut state = UiState {
        sessions: vec![info.clone()],
        ..UiState::default()
    };
    update(&mut state, UiEvent::SessionCreated(info.clone()));
    state.results.insert(
        info.id,
        bone_app::ResultPage {
            items: vec![result(&info)],
            older_cursor: None,
            snapshot_through: bone_app::SessionSeq(11),
            projection_pending: false,
        },
    );
    begin_acceptance(&mut state, bone_app::AcceptanceDecision::Rejected);
    if let Some(Dialog::Acceptance { reason, rework, .. }) = &mut state.dialog {
        *reason = "边界不完整".into();
        *rework = "补齐验证".into();
    }
    let first = update(&mut state, UiEvent::Action(Action::Submit));
    let Effect::SubmitAcceptance { submission, .. } = &first[0] else {
        panic!("expected first acceptance")
    };
    let first_request = submission.request_id;
    for action in [
        Action::Input('篡'),
        Action::Paste("不应写入".into()),
        Action::Backspace,
        Action::FocusNext,
        Action::FocusPrevious,
    ] {
        update(&mut state, UiEvent::Action(action));
    }
    assert!(matches!(
        &state.dialog,
        Some(Dialog::Acceptance {
            reason,
            rework,
            editing_rework: false,
            submitting: true,
            ..
        }) if reason == "边界不完整" && rework == "补齐验证"
    ));
    update(&mut state, UiEvent::Action(Action::Escape));
    begin_acceptance(&mut state, bone_app::AcceptanceDecision::Rejected);
    let active_request = match &state.dialog {
        Some(Dialog::Acceptance { request_id, .. }) => *request_id,
        _ => panic!("expected active dialog"),
    };
    assert_eq!(first_request, active_request);
    update(
        &mut state,
        UiEvent::AcceptanceSubmitted {
            session: info.id,
            generation: 1,
            request_id: first_request,
            receipt: bone_app::AcceptanceReceipt {
                id: bone_app::AcceptanceId::new(),
                saved_at: bone_app::SessionSeq(12),
                rework: None,
            },
        },
    );
    assert!(state.dialog.is_none());
}

#[test]
fn acceptance_stays_bound_to_its_original_session() {
    let first = session("first");
    let second = session("second");
    let mut state = UiState {
        sessions: vec![first.clone()],
        ..UiState::default()
    };
    update(&mut state, UiEvent::SessionCreated(first.clone()));
    state.results.insert(
        first.id,
        bone_app::ResultPage {
            items: vec![result(&first)],
            older_cursor: None,
            snapshot_through: bone_app::SessionSeq(11),
            projection_pending: false,
        },
    );
    begin_acceptance(&mut state, bone_app::AcceptanceDecision::Accepted);
    update(&mut state, UiEvent::SessionCreated(second));
    let effects = update(&mut state, UiEvent::Action(Action::Submit));
    assert!(matches!(
        &effects[0],
        Effect::SubmitAcceptance { session, .. } if *session == first.id
    ));
}

#[test]
fn a_new_watch_watermark_restarts_forward_history_after_the_old_tail() {
    let info = session("live");
    let mut ui = SessionUi::new(info.clone(), 1);
    ui.history_cursor = bone_app::SessionSeq(10);
    ui.history_has_more = false;
    ui.snapshot = Some(Arc::new(SessionView {
        session: info.clone(),
        runtime: bone_app::RuntimeState::Detached,
        draft: String::new(),
        inputs: Vec::new(),
        jobs: Vec::new(),
        activity: Vec::new(),
        history_through: bone_app::SessionSeq(10),
        problem: None,
    }));
    let mut state = UiState {
        sessions: vec![info.clone()],
        selected: Some(info.id),
        ..UiState::default()
    };
    state.session_ui.insert(info.id, ui);
    let changed = Arc::new(SessionView {
        history_through: bone_app::SessionSeq(11),
        ..state
            .selected_ui()
            .unwrap()
            .snapshot
            .as_ref()
            .unwrap()
            .as_ref()
            .clone()
    });
    let effects = update(
        &mut state,
        UiEvent::SessionChanged {
            session: info.id,
            generation: 1,
            snapshot: changed,
        },
    );
    assert!(matches!(
        &effects[0],
        Effect::LoadHistory { after, .. } if *after == bone_app::SessionSeq(10)
    ));
}

#[test]
fn full_history_cache_keeps_reading_anchor_when_live_data_arrives() {
    let info = session("anchor");
    let mut ui = SessionUi::new(info.clone(), 1);
    ui.history.extend((1..=512).map(history));
    ui.history_cursor = bone_app::SessionSeq(512);
    ui.scroll_from_tail = 450;
    let mut state = UiState {
        sessions: vec![info.clone()],
        selected: Some(info.id),
        ..UiState::default()
    };
    state.session_ui.insert(info.id, ui);
    let before_end = state.selected_ui().unwrap().history.len() - 450;
    let before = state.selected_ui().unwrap().history[before_end - 1].sequence;
    update(
        &mut state,
        UiEvent::HistoryLoaded {
            session: info.id,
            generation: 1,
            page: HistoryPage {
                items: vec![history(513)],
                next_cursor: bone_app::SessionSeq(513),
                has_more: false,
            },
        },
    );
    let ui = state.selected_ui().unwrap();
    let after_end = ui.history.len() - ui.scroll_from_tail;
    assert_eq!(ui.history[after_end - 1].sequence, before);
    assert!(ui.newer_history_missing);
}

#[test]
fn receipt_never_deletes_retyped_text_that_only_looks_like_the_old_draft() {
    let info = session("lineage");
    let mut state = UiState {
        sessions: vec![info.clone()],
        ..UiState::default()
    };
    update(&mut state, UiEvent::SessionCreated(info.clone()));
    mark_selected_open(&mut state);
    update(&mut state, UiEvent::Action(Action::Input('A')));
    let submit = update(&mut state, UiEvent::Action(Action::Submit));
    let Effect::Submit {
        generation, input, ..
    } = &submit[0]
    else {
        panic!("expected submit")
    };
    let generation = *generation;
    let request_id = input.request_id;
    update(&mut state, UiEvent::Action(Action::Backspace));
    update(&mut state, UiEvent::Action(Action::Input('A')));
    update(
        &mut state,
        UiEvent::Submitted {
            session: info.id,
            generation,
            request_id,
            receipt: SubmissionReceipt {
                input: bone_app::InputId(1),
                saved_at: bone_app::SessionSeq(1),
            },
        },
    );
    assert_eq!(state.selected_ui().unwrap().draft, "A");
}

#[test]
fn visible_settings_actions_use_app_config_and_login_effects() {
    let info = session("settings");
    let mut state = UiState {
        sessions: vec![info.clone()],
        main: MainView::Settings,
        ..UiState::default()
    };
    update(&mut state, UiEvent::SessionCreated(info.clone()));
    mark_selected_open(&mut state);
    state.main = MainView::Settings;
    state.settings = Some(SettingsData {
        session: info.id,
        generation: 1,
        query: 0,
        resolved: bone_app::ResolvedConfig {
            desired: Err(bone_app::ConfigProblem::NeedsModel),
            running: None,
        },
        profiles: vec![bone_app::Profile::chatgpt()],
    });

    let login = update(&mut state, UiEvent::Action(Action::StartLogin));
    assert!(matches!(
        &login[0],
        Effect::StartLogin { profile, query: 1 } if *profile == bone_app::ProfileId::chatgpt()
    ));
    update(
        &mut state,
        UiEvent::Action(Action::ConfigureModel(ModelRole::Worker)),
    );
    update(
        &mut state,
        UiEvent::Action(Action::Paste("gpt-test".into())),
    );
    let save = update(&mut state, UiEvent::Action(Action::Submit));
    assert!(matches!(
        &save[0],
        Effect::UpdateConfig {
            session,
            change: bone_app::ConfigChange::Worker(Some(selection)),
            ..
        } if *session == info.id && selection.model == "gpt-test"
    ));
}

#[test]
fn stale_settings_cannot_act_on_the_selected_session() {
    let first = session("first");
    let second = session("second");
    let mut state = UiState {
        sessions: vec![first.clone(), second.clone()],
        selected: Some(second.id),
        main: MainView::Settings,
        settings: Some(SettingsData {
            session: first.id,
            generation: 1,
            query: 0,
            resolved: bone_app::ResolvedConfig {
                desired: Err(bone_app::ConfigProblem::NeedsModel),
                running: None,
            },
            profiles: vec![bone_app::Profile::chatgpt()],
        }),
        ..UiState::default()
    };
    state
        .session_ui
        .insert(first.id, SessionUi::new(first.clone(), 1));
    state
        .session_ui
        .insert(second.id, SessionUi::new(second, 2));

    assert!(
        update(
            &mut state,
            UiEvent::Action(Action::ConfigureModel(ModelRole::Worker))
        )
        .is_empty()
    );
    assert!(state.dialog.is_none());
    assert!(update(&mut state, UiEvent::Action(Action::StartLogin)).is_empty());
}

#[test]
fn rename_response_is_bound_to_session_generation_and_operation() {
    let first = session("first");
    let second = session("second");
    let mut state = UiState {
        sessions: vec![first.clone(), second.clone()],
        selected: Some(first.id),
        main: MainView::Sessions,
        ..UiState::default()
    };
    state
        .session_ui
        .insert(first.id, SessionUi::new(first.clone(), 11));
    state
        .session_ui
        .insert(second.id, SessionUi::new(second.clone(), 22));

    update(
        &mut state,
        UiEvent::Action(Action::BeginRenameSession(first.id)),
    );
    let operation = match &mut state.dialog {
        Some(Dialog::RenameSession {
            operation, title, ..
        }) => {
            *title = "renamed first".into();
            *operation
        }
        other => panic!("expected rename dialog, got {other:?}"),
    };
    let effects = update(&mut state, UiEvent::Action(Action::Submit));
    assert!(matches!(
        &effects[0],
        Effect::RenameSession {
            session,
            generation: 11,
            operation: active,
            title,
        } if *session == first.id && *active == operation && title == "renamed first"
    ));

    // A response carrying the same numeric operation but another session
    // must neither close the first session's dialog nor rename either row.
    state.selected = Some(second.id);
    update(
        &mut state,
        UiEvent::SessionRenamed {
            session: second.id,
            generation: 22,
            operation,
            title: "wrong target".into(),
        },
    );
    assert!(matches!(state.dialog, Some(Dialog::RenameSession { .. })));
    assert_eq!(state.sessions[0].title, "first");
    assert_eq!(state.sessions[1].title, "second");

    update(
        &mut state,
        UiEvent::SessionRenamed {
            session: first.id,
            generation: 11,
            operation,
            title: "renamed first".into(),
        },
    );
    assert!(state.dialog.is_none());
    assert_eq!(state.sessions[0].title, "renamed first");
    assert_eq!(state.sessions[1].title, "second");
}

#[test]
fn stale_archive_response_cannot_overwrite_a_newer_operation() {
    let info = session("archive");
    let mut state = UiState {
        sessions: vec![info.clone()],
        selected: Some(info.id),
        main: MainView::Sessions,
        ..UiState::default()
    };
    state
        .session_ui
        .insert(info.id, SessionUi::new(info.clone(), 7));

    let first = update(
        &mut state,
        UiEvent::Action(Action::SetSessionArchived {
            session: info.id,
            archived: true,
        }),
    );
    let first_operation = match first[0] {
        Effect::ArchiveSession { operation, .. } => operation,
        ref other => panic!("expected archive effect, got {other:?}"),
    };
    // Model a newer user operation after the first request has completed
    // at the App boundary but before its UI event is delivered.
    state.session_management_operations.remove(&info.id);
    let second = update(
        &mut state,
        UiEvent::Action(Action::SetSessionArchived {
            session: info.id,
            archived: false,
        }),
    );
    let second_operation = match second[0] {
        Effect::ArchiveSession { operation, .. } => operation,
        ref other => panic!("expected restore effect, got {other:?}"),
    };
    assert_ne!(first_operation, second_operation);

    update(
        &mut state,
        UiEvent::SessionArchived {
            session: info.id,
            generation: 7,
            operation: first_operation,
            archived: true,
        },
    );
    assert!(!state.sessions[0].archived);
    update(
        &mut state,
        UiEvent::SessionArchived {
            session: info.id,
            generation: 7,
            operation: second_operation,
            archived: false,
        },
    );
    assert!(!state.sessions[0].archived);
}

#[test]
fn model_dialog_keeps_its_original_session_identity() {
    let first = session("first");
    let second = session("second");
    let mut state = UiState {
        sessions: vec![first.clone(), second.clone()],
        selected: Some(first.id),
        main: MainView::Settings,
        settings: Some(SettingsData {
            session: first.id,
            generation: 7,
            query: 0,
            resolved: bone_app::ResolvedConfig {
                desired: Err(bone_app::ConfigProblem::NeedsModel),
                running: None,
            },
            profiles: vec![bone_app::Profile::chatgpt()],
        }),
        ..UiState::default()
    };
    state
        .session_ui
        .insert(first.id, SessionUi::new(first.clone(), 7));
    state
        .session_ui
        .insert(second.id, SessionUi::new(second.clone(), 8));
    update(
        &mut state,
        UiEvent::Action(Action::ConfigureModel(ModelRole::Worker)),
    );
    state.selected = Some(second.id);
    update(
        &mut state,
        UiEvent::Action(Action::Paste("gpt-test\ninjected".into())),
    );
    let effects = update(&mut state, UiEvent::Action(Action::Submit));
    assert!(matches!(
        &effects[0],
        Effect::UpdateConfig {
            session,
            generation: 7,
            change: bone_app::ConfigChange::Worker(Some(selection)),
        } if *session == first.id && selection.model == "gpt-testinjected"
    ));
}

#[test]
fn successful_login_reloads_open_sessions_and_current_settings() {
    let info = session("login");
    let mut state = UiState {
        sessions: vec![info.clone()],
        selected: Some(info.id),
        ..UiState::default()
    };
    state
        .session_ui
        .insert(info.id, SessionUi::new(info.clone(), 3));
    state
        .login_queries
        .insert(bone_app::ProfileId::chatgpt(), 4);
    let effects = update(
        &mut state,
        UiEvent::LoginChanged {
            profile: bone_app::ProfileId::chatgpt(),
            query: 4,
            state: bone_app::LoginState::Succeeded,
        },
    );
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::ReloadOpenSessions { .. }))
    );
    assert!(effects.iter().any(|effect| matches!(
        effect,
        Effect::LoadSettings { session, generation: 3, .. } if *session == info.id
    )));
}

#[test]
fn detail_has_independent_scroll_state() {
    let mut state = UiState {
        focus: Focus::Detail,
        detail: Some(DetailState {
            kind: DetailKind::Acceptance,
            title: "结果".into(),
        }),
        ..UiState::default()
    };
    update(&mut state, UiEvent::Action(Action::ScrollDown(7)));
    assert_eq!(state.detail_scroll, 7);
    update(&mut state, UiEvent::Action(Action::ScrollUp(2)));
    assert_eq!(state.detail_scroll, 5);
}

#[test]
fn stale_release_receipt_cannot_close_a_reopened_session_generation() {
    let info = session("release-race");
    let mut state = UiState {
        sessions: vec![info.clone()],
        selected: Some(info.id),
        ..UiState::default()
    };
    let mut ui = SessionUi::new(info.clone(), 9);
    ui.snapshot = Some(snapshot(&info, ""));
    state.session_ui.insert(info.id, ui);

    let effects = update(
        &mut state,
        UiEvent::SessionReleased {
            generation: 8,
            receipt: bone_app::SessionReleaseReceipt {
                session: info.id,
                status: bone_app::SessionReleaseStatus::Released,
            },
        },
    );
    assert!(effects.is_empty());
    assert!(state.selected_ui().unwrap().snapshot.is_some());
    assert_eq!(state.selected_ui().unwrap().generation, 9);
}

#[test]
fn release_rehydrate_does_not_duplicate_saved_draft_or_history() {
    let info = session("rehydrate");
    let mut ui = SessionUi::new(info.clone(), 1);
    ui.snapshot = Some(snapshot(&info, "x"));
    ui.hydrated_once = true;
    ui.draft = "x".into();
    ui.saved_draft = "x".into();
    ui.draft_revision = 1;
    ui.saved_draft_revision = 1;
    ui.queued_draft_revision = 1;
    ui.history.push_back(history(1));
    let mut state = UiState {
        sessions: vec![info.clone()],
        session_ui: BTreeMap::from([(info.id, ui)]),
        next_generation: 10,
        ..UiState::default()
    };
    update(
        &mut state,
        UiEvent::SessionReleased {
            generation: 1,
            receipt: bone_app::SessionReleaseReceipt {
                session: info.id,
                status: bone_app::SessionReleaseStatus::Released,
            },
        },
    );
    let open = update(&mut state, UiEvent::Action(Action::SelectSession(info.id)));
    let generation = match open.as_slice() {
        [Effect::OpenSession { generation, .. }] => *generation,
        other => panic!("expected one reopen, got {other:?}"),
    };
    update(
        &mut state,
        UiEvent::SessionOpened {
            session: info.id,
            generation,
            snapshot: snapshot(&info, "x"),
            history: RecentHistoryPage {
                items: vec![history(1)],
                older_cursor: None,
                snapshot_through: bone_app::SessionSeq(1),
            },
        },
    );
    let ui = state.selected_ui().unwrap();
    assert_eq!(ui.draft, "x");
    assert_eq!(ui.history.len(), 1);
}

#[test]
fn release_during_a_new_debounced_edit_requeues_that_draft_after_reopen() {
    let info = session("release-edit");
    let mut ui = SessionUi::new(info.clone(), 1);
    ui.snapshot = Some(snapshot(&info, "x"));
    ui.hydrated_once = true;
    ui.draft = "xy".into();
    ui.saved_draft = "x".into();
    ui.draft_revision = 2;
    ui.saved_draft_revision = 1;
    ui.queued_draft_revision = 2;
    let mut state = UiState {
        sessions: vec![info.clone()],
        selected: Some(info.id),
        session_ui: BTreeMap::from([(info.id, ui)]),
        next_generation: 10,
        ..UiState::default()
    };
    let released = update(
        &mut state,
        UiEvent::SessionReleased {
            generation: 1,
            receipt: bone_app::SessionReleaseReceipt {
                session: info.id,
                status: bone_app::SessionReleaseStatus::Released,
            },
        },
    );
    let generation = match released.as_slice() {
        [Effect::OpenSession { generation, .. }] => *generation,
        other => panic!("expected selected session reopen, got {other:?}"),
    };
    let effects = update(
        &mut state,
        UiEvent::SessionOpened {
            session: info.id,
            generation,
            // The old save reached App, but its UI receipt lost the race
            // with release. Rehydration must not append the full local
            // buffer to the now-identical durable buffer.
            snapshot: snapshot(&info, "xy"),
            history: RecentHistoryPage {
                items: Vec::new(),
                older_cursor: None,
                snapshot_through: bone_app::SessionSeq(0),
            },
        },
    );
    assert!(matches!(
        effects.as_slice(),
        [Effect::SaveDraft {
            session,
            generation: saved_generation,
            revision: 2,
            text,
        }] if *session == info.id && *saved_generation == generation && text == "xy"
    ));
}

#[test]
fn attention_question_routes_a_typed_answer_to_the_exact_question() {
    let info = session("question");
    let runtime = bone_app::RuntimeId::new();
    let question = QuestionId {
        runtime,
        record: 12,
        reply_to: bone_app::InputId(4),
    };
    let mut state = UiState {
        sessions: vec![info.clone()],
        main: MainView::Attention,
        attention: vec![AttentionItem::WaitingForUser {
            session: info.id,
            inputs: vec![bone_app::InputId(4)],
            runtime,
            question,
            text: "继续吗？".into(),
        }],
        ..UiState::default()
    };
    update(&mut state, UiEvent::Action(Action::OpenAttention(0)));
    mark_selected_open(&mut state);
    update(&mut state, UiEvent::Action(Action::Input('是')));
    let effects = update(&mut state, UiEvent::Action(Action::Submit));
    assert!(matches!(
        effects.as_slice(),
        [Effect::Submit { input, .. }] if input.reply_to == Some(question) && input.text == "是"
    ));
}

#[test]
fn unknown_write_resolution_requires_evidence_and_uses_the_exact_call() {
    let info = session("write");
    let call = bone_app::CallRef {
        runtime: bone_app::RuntimeId::new(),
        id: 8,
    };
    let item = AttentionItem::UnresolvedWrite {
        session: info.id,
        call,
        status: bone_app::UnresolvedWriteStatus::Finished,
    };
    let mut state = UiState {
        attention: vec![item],
        ..UiState::default()
    };
    update(&mut state, UiEvent::Action(Action::OpenAttention(0)));
    update(
        &mut state,
        UiEvent::Action(Action::BeginWriteResolution(
            bone_app::ExternalEffect::Applied,
        )),
    );
    assert!(update(&mut state, UiEvent::Action(Action::Submit)).is_empty());
    update(
        &mut state,
        UiEvent::Action(Action::Paste("已在外部核对".into())),
    );
    let effects = update(&mut state, UiEvent::Action(Action::Submit));
    assert!(matches!(
        effects.as_slice(),
        [Effect::ResolveWrite {
            session,
            call: target,
            resolution,
        }] if *session == info.id
            && *target == call
            && resolution.external_effect == bone_app::ExternalEffect::Applied
            && resolution.evidence == "已在外部核对"
    ));
}

#[test]
fn workspace_changes_keep_app_identity_and_use_working_tree_for_untracked_files() {
    let workspace = WorkspaceId::new();
    let mut state = UiState {
        workspace: Some((workspace, "project".into())),
        ..UiState::default()
    };
    let effects = update(
        &mut state,
        UiEvent::Action(Action::OpenDetail(DetailState {
            kind: DetailKind::Changes,
            title: "工作区变更".into(),
        })),
    );
    let [
        Effect::LoadWorkspaceChanges {
            workspace: requested_workspace,
            query,
            cursor: None,
            append: false,
        },
    ] = effects.as_slice()
    else {
        panic!("opening Changes must query the App")
    };
    assert_eq!(*requested_workspace, workspace);
    let query = *query;

    update(
        &mut state,
        UiEvent::WorkspaceChangesLoaded {
            workspace,
            query,
            page: bone_app::WorkspaceChangePage {
                baseline: bone_app::WorkspaceBaseline::Git {
                    head: Some("abc123".into()),
                },
                files: vec![bone_app::WorkspaceChangedFile {
                    path: "new.txt".into(),
                    tracked: false,
                    index: bone_app::GitFileState::Untracked,
                    worktree: bone_app::GitFileState::Untracked,
                }],
                next_cursor: None,
            },
            append: false,
        },
    );
    let effects = update(&mut state, UiEvent::Action(Action::Activate));
    assert!(matches!(
        effects.as_slice(),
        [Effect::LoadWorkspaceFile {
            workspace: requested_workspace,
            path,
            source: bone_app::WorkspaceFileSource::WorkingTree,
            ..
        }] if *requested_workspace == workspace && path == "new.txt"
    ));
}

#[test]
fn changed_git_baseline_releases_the_forward_request_before_reloading() {
    let workspace = WorkspaceId::new();
    let mut state = UiState {
        workspace: Some((workspace, "project".into())),
        ..UiState::default()
    };
    let effects = update(
        &mut state,
        UiEvent::Action(Action::OpenDetail(DetailState {
            kind: DetailKind::Changes,
            title: "工作区变更".into(),
        })),
    );
    let Effect::LoadWorkspaceChanges { query, .. } = effects[0] else {
        panic!("expected initial workspace request")
    };
    update(
        &mut state,
        UiEvent::WorkspaceChangesLoaded {
            workspace,
            query,
            page: bone_app::WorkspaceChangePage {
                baseline: bone_app::WorkspaceBaseline::Git {
                    head: Some("old".into()),
                },
                files: Vec::new(),
                next_cursor: None,
            },
            append: false,
        },
    );

    assert!(
        state
            .workspace_changes
            .pages
            .begin(None, PageDirection::Forward)
    );
    state.workspace_changes.loading = true;
    let effects = update(
        &mut state,
        UiEvent::WorkspaceChangesLoaded {
            workspace,
            query,
            page: bone_app::WorkspaceChangePage {
                baseline: bone_app::WorkspaceBaseline::Git {
                    head: Some("new".into()),
                },
                files: Vec::new(),
                next_cursor: None,
            },
            append: true,
        },
    );

    assert!(matches!(
        effects.as_slice(),
        [Effect::LoadWorkspaceChanges {
            cursor: None,
            append: false,
            ..
        }]
    ));
    assert!(state.workspace_changes.loading);
    assert!(state.workspace_changes.pages.pending.is_some());
}

#[test]
fn stale_workspace_file_never_replaces_the_current_change_selection() {
    let workspace = WorkspaceId::new();
    let mut state = UiState {
        workspace: Some((workspace, "project".into())),
        detail: Some(DetailState {
            kind: DetailKind::Changes,
            title: "工作区变更".into(),
        }),
        ..UiState::default()
    };
    state.workspace_changes.file_query = 9;
    update(
        &mut state,
        UiEvent::WorkspaceFileLoaded {
            workspace,
            query: 8,
            cursor: None,
            append: false,
            page: bone_app::WorkspaceFilePage {
                baseline: bone_app::WorkspaceBaseline::NotGit,
                path: "stale.txt".into(),
                source: bone_app::WorkspaceFileSource::WorkingTree,
                media: bone_app::WorkspaceFileMedia::Text,
                text: Some("stale".into()),
                offset: 0,
                bytes_read: 5,
                total_bytes: Some(5),
                next_cursor: None,
            },
        },
    );
    assert!(state.workspace_changes.file.is_none());
}

#[test]
fn workspace_file_page_failures_are_bound_to_the_exact_window_request() {
    let workspace = WorkspaceId::new();
    let mut state = UiState {
        workspace: Some((workspace, "project".into())),
        detail: Some(DetailState {
            kind: DetailKind::Changes,
            title: "工作区变更".into(),
        }),
        ..UiState::default()
    };
    state.workspace_changes.page = Some(bone_app::WorkspaceChangePage {
        baseline: bone_app::WorkspaceBaseline::NotGit,
        files: vec![bone_app::WorkspaceChangedFile {
            path: "large.txt".into(),
            tracked: false,
            index: bone_app::GitFileState::Untracked,
            worktree: bone_app::GitFileState::Untracked,
        }],
        next_cursor: None,
    });

    let effects = update(&mut state, UiEvent::Action(Action::OpenWorkspaceChange(0)));
    let [Effect::LoadWorkspaceFile { query, .. }] = effects.as_slice() else {
        panic!("opening a workspace file must query the App")
    };
    let query = *query;
    update(
        &mut state,
        UiEvent::WorkspaceFileLoaded {
            workspace,
            query,
            cursor: None,
            append: false,
            page: bone_app::WorkspaceFilePage {
                baseline: bone_app::WorkspaceBaseline::NotGit,
                path: "large.txt".into(),
                source: bone_app::WorkspaceFileSource::WorkingTree,
                media: bone_app::WorkspaceFileMedia::Text,
                text: Some("second window".into()),
                offset: 262_144,
                bytes_read: 13,
                total_bytes: Some(262_157),
                next_cursor: None,
            },
        },
    );
    state.workspace_changes.file_back.push(None);
    let effects = update(
        &mut state,
        UiEvent::Action(Action::LoadPreviousWorkspaceFile),
    );
    let [
        Effect::LoadWorkspaceFile {
            query,
            path,
            cursor,
            append,
            ..
        },
    ] = effects.as_slice()
    else {
        panic!("returning to the first window must query the App")
    };
    assert_eq!(path, "large.txt");
    assert!(cursor.is_none());
    assert!(!append);
    let query = *query;

    let effects = update(
        &mut state,
        UiEvent::WorkspaceFileFailed {
            workspace,
            query,
            path: "other.txt".into(),
            source: bone_app::WorkspaceFileSource::WorkingTree,
            cursor: None,
            append: false,
            message: "late wrong file".into(),
        },
    );
    assert!(effects.is_empty());
    assert!(state.workspace_changes.file_loading);
    assert_eq!(
        state
            .workspace_changes
            .file
            .as_ref()
            .map(|page| page.path.as_str()),
        Some("large.txt")
    );

    let effects = update(
        &mut state,
        UiEvent::WorkspaceFileFailed {
            workspace,
            query,
            path: "large.txt".into(),
            source: bone_app::WorkspaceFileSource::WorkingTree,
            cursor: None,
            append: false,
            message: "file changed".into(),
        },
    );
    assert!(state.workspace_changes.file.is_none());
    assert!(matches!(
        effects.as_slice(),
        [Effect::LoadWorkspaceChanges {
            workspace: requested_workspace,
            cursor: None,
            append: false,
            ..
        }] if *requested_workspace == workspace
    ));
    assert!(
        state
            .status
            .as_deref()
            .is_some_and(|message| message.contains("正在刷新文件列表"))
    );
}

#[test]
fn artifact_navigation_stays_bound_to_the_exact_result_and_evidence_reference() {
    let info = session("artifact");
    let mut state = UiState {
        sessions: vec![info.clone()],
        ..UiState::default()
    };
    update(&mut state, UiEvent::SessionCreated(info.clone()));
    let generation = state.session_ui[&info.id].generation;
    let effects = update(
        &mut state,
        UiEvent::Action(Action::OpenDetail(DetailState {
            kind: DetailKind::Artifacts,
            title: "产物与证据".into(),
        })),
    );
    let result_query = effects
        .iter()
        .find_map(|effect| match effect {
            Effect::LoadResults { query, .. } => Some(*query),
            _ => None,
        })
        .expect("opening artifacts loads durable results first");
    let summary = result(&info);
    let effects = update(
        &mut state,
        UiEvent::ResultsLoaded {
            session: info.id,
            generation,
            query: result_query,
            page: bone_app::ResultPage {
                items: vec![summary.clone()],
                older_cursor: None,
                snapshot_through: summary.result.version,
                projection_pending: false,
            },
        },
    );
    let artifact_query = effects
        .iter()
        .find_map(|effect| match effect {
            Effect::LoadArtifact { result, query } if *result == summary.result => Some(*query),
            _ => None,
        })
        .expect("latest exact result becomes the artifact target");
    let source = bone_app::EvidenceRef {
        session: info.id,
        record: 42,
    };
    update(
        &mut state,
        UiEvent::ArtifactLoaded {
            result: summary.result,
            query: artifact_query,
            artifact: bone_app::ResultArtifact {
                result: summary.result,
                outcome: summary.outcome,
                summary: summary.summary,
                remaining: summary.remaining,
                evidence_count: 1,
            },
            evidence: bone_app::EvidencePage {
                result: summary.result,
                items: vec![bone_app::EvidenceSummary {
                    source,
                    availability: bone_app::EvidenceAvailability::Available {
                        kind: bone_app::EvidenceSourceKind::Reply,
                        title: "最终回复".into(),
                    },
                }],
                next_cursor: None,
                projection_pending: false,
            },
        },
    );
    let effects = update(&mut state, UiEvent::Action(Action::Activate));
    assert!(matches!(
        effects.as_slice(),
        [Effect::LoadEvidenceSource {
            result,
            source: requested_source,
            offset: 0,
            append: false,
            ..
        }] if *result == summary.result && *requested_source == source
    ));
}

#[test]
fn write_resolution_evidence_is_charged_to_the_shared_cache_budget() {
    let info = session("large evidence history");
    let mut state = UiState {
        sessions: vec![info.clone()],
        ..UiState::default()
    };
    update(&mut state, UiEvent::SessionCreated(info.clone()));
    let generation = state.session_ui[&info.id].generation;
    let items = (1..=40)
        .map(|sequence| HistoryEntry {
            sequence: bone_app::SessionSeq(sequence),
            occurred_at: sequence as i64,
            event: bone_app::SessionEvent::WriteResolved {
                call: bone_app::CallRef {
                    runtime: bone_app::RuntimeId::new(),
                    id: sequence,
                },
                external_effect: bone_app::ExternalEffect::Applied,
                evidence: "e".repeat(1024 * 1024),
            },
        })
        .collect();

    update(
        &mut state,
        UiEvent::HistoryLoaded {
            session: info.id,
            generation,
            page: HistoryPage {
                items,
                next_cursor: bone_app::SessionSeq(40),
                has_more: false,
            },
        },
    );

    let ui = &state.session_ui[&info.id];
    assert!(ui.history_bytes <= HISTORY_CACHE_BYTES);
    assert!(ui.history.len() < 40, "large evidence must force eviction");
    assert!(ui.history_window_stale);
}

#[test]
fn result_pages_cross_thirty_two_mib_without_accumulating_and_can_go_back() {
    let info = session("large results");
    let mut state = UiState {
        sessions: vec![info.clone()],
        ..UiState::default()
    };
    update(&mut state, UiEvent::SessionCreated(info.clone()));
    let runtime = bone_app::RuntimeId::new();
    let generation = state.session_ui[&info.id].generation;
    let query = state.session_ui[&info.id].result_query;
    let mut traversed = 0usize;
    for version in (1..=40).rev() {
        let page = bone_app::ResultPage {
            items: vec![bone_app::ResultSummary {
                result: bone_app::ResultRef {
                    session: info.id,
                    job: bone_app::JobRef {
                        runtime,
                        id: version,
                    },
                    version: bone_app::SessionSeq(version),
                },
                outcome: bone_app::OutcomeKind::Completed,
                summary: "r".repeat(1024 * 1024),
                remaining: Vec::new(),
            }],
            older_cursor: None,
            snapshot_through: bone_app::SessionSeq(40),
            projection_pending: false,
        };
        traversed += page.items[0].summary.len();
        state
            .session_ui
            .get_mut(&info.id)
            .unwrap()
            .result_pages
            .begin(
                None,
                if version == 40 {
                    PageDirection::Refresh
                } else {
                    PageDirection::Forward
                },
            );
        update(
            &mut state,
            if version == 40 {
                UiEvent::ResultsLoaded {
                    session: info.id,
                    generation,
                    query,
                    page,
                }
            } else {
                UiEvent::OlderResultsLoaded {
                    session: info.id,
                    generation,
                    query,
                    page,
                }
            },
        );
    }
    assert!(traversed > HISTORY_CACHE_BYTES);
    assert_eq!(state.results[&info.id].items.len(), 1);
    assert_eq!(state.session_ui[&info.id].result_pages.back.len(), 39);

    let effects = update(&mut state, UiEvent::Action(Action::LoadNewerResults));
    assert!(matches!(
        effects.as_slice(),
        [Effect::LoadResults { session, .. }] if *session == info.id
    ));
}

#[test]
fn duplicate_result_delivery_does_not_flood_acceptance_requests() {
    let info = session("acceptance inflight");
    let summary = result(&info);
    let page = bone_app::ResultPage {
        items: vec![summary.clone()],
        older_cursor: None,
        snapshot_through: summary.result.version,
        projection_pending: false,
    };
    let mut state = UiState {
        sessions: vec![info.clone()],
        ..UiState::default()
    };
    update(&mut state, UiEvent::SessionCreated(info.clone()));
    let generation = state.session_ui[&info.id].generation;
    let query = state.session_ui[&info.id].result_query;

    let first = update(
        &mut state,
        UiEvent::ResultsLoaded {
            session: info.id,
            generation,
            query,
            page: page.clone(),
        },
    );
    let second = update(
        &mut state,
        UiEvent::ResultsLoaded {
            session: info.id,
            generation,
            query,
            page,
        },
    );

    assert_eq!(
        first
            .iter()
            .filter(|effect| matches!(effect, Effect::LoadAcceptances { .. }))
            .count(),
        1
    );
    assert!(
        second
            .iter()
            .all(|effect| !matches!(effect, Effect::LoadAcceptances { .. }))
    );
}

#[test]
fn acceptance_pages_cross_thirty_two_mib_without_accumulating_and_can_go_back() {
    let info = session("acceptance cache reread");
    let summary = result(&info);
    let mut state = UiState {
        sessions: vec![info.clone()],
        ..UiState::default()
    };
    update(&mut state, UiEvent::SessionCreated(info.clone()));
    state.results.insert(
        info.id,
        bone_app::ResultPage {
            items: vec![summary.clone()],
            older_cursor: None,
            snapshot_through: summary.result.version,
            projection_pending: false,
        },
    );
    let generation = state.session_ui[&info.id].generation;
    state.acceptance_queries.insert(summary.result, 1);
    let mut traversed = 0usize;
    for saved_at in (1..=40).rev() {
        let page = bone_app::AcceptancePage {
            items: vec![bone_app::AcceptanceRecord {
                id: bone_app::AcceptanceId::new(),
                request_id: bone_app::AcceptanceRequestId::new(),
                result: summary.result,
                decision: bone_app::AcceptanceDecision::Accepted,
                reason: "a".repeat(1024 * 1024),
                saved_at: bone_app::SessionSeq(saved_at),
                rework: None,
            }],
            older_cursor: None,
        };
        traversed += page.items[0].reason.len();
        state
            .acceptance_pages
            .entry(summary.result)
            .or_default()
            .begin(
                None,
                if saved_at == 40 {
                    PageDirection::Refresh
                } else {
                    PageDirection::Forward
                },
            );
        update(
            &mut state,
            UiEvent::AcceptancesLoaded {
                session: info.id,
                generation,
                result: summary.result,
                query: 1,
                page,
                append_older: saved_at != 40,
            },
        );
    }
    assert!(traversed > HISTORY_CACHE_BYTES);
    assert_eq!(state.acceptances[&summary.result].items.len(), 1);
    assert_eq!(state.acceptance_pages[&summary.result].back.len(), 39);

    let effects = update(&mut state, UiEvent::Action(Action::LoadNewerAcceptances));
    assert!(matches!(
        effects.as_slice(),
        [Effect::LoadAcceptances {
            result,
            cursor: None,
            append_older: false,
            ..
        }] if *result == summary.result
    ));
}

#[test]
fn evidence_window_can_reread_the_previous_app_page_after_sliding() {
    let info = session("evidence window");
    let result = result(&info).result;
    let source = bone_app::EvidenceRef {
        session: info.id,
        record: 9,
    };
    let mut state = UiState::default();
    state.artifact.source = Some(EvidenceReaderUi {
        result,
        source,
        availability: bone_app::EvidenceAvailability::Available {
            kind: bone_app::EvidenceSourceKind::Reply,
            title: "reply".into(),
        },
        text: "visible window".into(),
        window_offset: (update::EVIDENCE_VIEW_BYTES * 2) as u64,
        next_offset: Some((update::EVIDENCE_VIEW_BYTES * 3) as u64),
        total_bytes: Some((update::EVIDENCE_VIEW_BYTES * 4) as u64),
        projection_pending: false,
        frontend_truncated: true,
        layout: Default::default(),
    });

    let effects = update(
        &mut state,
        UiEvent::Action(Action::LoadPreviousEvidenceSource),
    );

    assert!(matches!(
        effects.as_slice(),
        [Effect::LoadEvidenceSource {
            result: requested_result,
            source: requested_source,
            offset,
            append: false,
            ..
        }] if *requested_result == result
            && *requested_source == source
            && *offset == update::EVIDENCE_VIEW_BYTES as u64
    ));
}

#[test]
fn workspace_change_pages_cross_thirty_two_mib_and_return_to_the_previous_page() {
    let workspace = WorkspaceId::new();
    let baseline = bone_app::WorkspaceBaseline::Git {
        head: Some("0123456789abcdef".into()),
    };
    let mut state = UiState {
        workspace: Some((workspace, "large changes".into())),
        ..UiState::default()
    };
    state.workspace_changes.query = 7;
    let mut traversed = 0usize;
    for page_number in 0..40 {
        let path = format!("{}-{page_number}", "p".repeat(1024 * 1024));
        traversed += path.len();
        state.workspace_changes.pages.begin(
            None,
            if page_number == 0 {
                PageDirection::Refresh
            } else {
                PageDirection::Forward
            },
        );
        update(
            &mut state,
            UiEvent::WorkspaceChangesLoaded {
                workspace,
                query: 7,
                page: bone_app::WorkspaceChangePage {
                    baseline: baseline.clone(),
                    files: vec![bone_app::WorkspaceChangedFile {
                        path,
                        tracked: true,
                        index: bone_app::GitFileState::Modified,
                        worktree: bone_app::GitFileState::Modified,
                    }],
                    next_cursor: None,
                },
                append: page_number != 0,
            },
        );
    }
    assert!(traversed > HISTORY_CACHE_BYTES);
    assert_eq!(
        state.workspace_changes.page.as_ref().unwrap().files.len(),
        1
    );
    assert_eq!(state.workspace_changes.pages.back.len(), 39);

    let effects = update(
        &mut state,
        UiEvent::Action(Action::LoadNewerWorkspaceChanges),
    );
    assert!(matches!(
        effects.as_slice(),
        [Effect::LoadWorkspaceChanges {
            workspace: requested,
            cursor: None,
            append: false,
            ..
        }] if *requested == workspace
    ));
}

#[test]
fn evidence_metadata_pages_cross_thirty_two_mib_and_return_to_the_previous_page() {
    let info = session("large evidence pages");
    let result = result(&info).result;
    let mut state = UiState {
        selected: Some(info.id),
        ..UiState::default()
    };
    state.artifact.query = 9;
    state.artifact.artifact = Some(bone_app::ResultArtifact {
        result,
        outcome: bone_app::OutcomeKind::Completed,
        summary: "artifact".into(),
        remaining: Vec::new(),
        evidence_count: 40,
    });
    let mut traversed = 0usize;
    for page_number in 0..40 {
        let title = format!("{}-{page_number}", "t".repeat(1024 * 1024));
        traversed += title.len();
        state.artifact.evidence_pages.begin(
            None,
            if page_number == 0 {
                PageDirection::Refresh
            } else {
                PageDirection::Forward
            },
        );
        update(
            &mut state,
            UiEvent::EvidenceLoaded {
                result,
                query: 9,
                page: bone_app::EvidencePage {
                    result,
                    items: vec![bone_app::EvidenceSummary {
                        source: bone_app::EvidenceRef {
                            session: info.id,
                            record: page_number,
                        },
                        availability: bone_app::EvidenceAvailability::Available {
                            kind: bone_app::EvidenceSourceKind::Reply,
                            title,
                        },
                    }],
                    next_cursor: None,
                    projection_pending: false,
                },
            },
        );
    }
    assert!(traversed > HISTORY_CACHE_BYTES);
    assert_eq!(state.artifact.evidence.as_ref().unwrap().items.len(), 1);
    assert_eq!(state.artifact.evidence_pages.back.len(), 39);

    let effects = update(&mut state, UiEvent::Action(Action::LoadNewerEvidence));
    assert!(matches!(
        effects.as_slice(),
        [Effect::LoadArtifact { result: requested, .. }] if *requested == result
    ));
}
