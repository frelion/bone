use super::*;

#[test]
fn changing_a_model_keeps_endpoint_and_does_not_move_credentials_between_providers() {
    let current: Profile = serde_json::from_value(serde_json::json!({
        "model":{"model":"old","config":{"openai":{"api_key":"","base_url":"http://127.0.0.1:1234/v1","dialect":"openai","route":"Responses","auth":"Bearer"}}},
        "credential_env":"LOCAL_FIXTURE_KEY","max_tokens":4000
    })).unwrap();
    let next = profile_with_model(&current, "openai:new-model").unwrap();
    let encoded = serde_json::to_value(&next).unwrap();
    assert_eq!(
        encoded["model"]["config"]["openai"]["base_url"],
        "http://127.0.0.1:1234/v1"
    );
    assert_eq!(encoded["model"]["model"], "new-model");
    assert_eq!(next.credential_env, current.credential_env);
    let changed = profile_with_model(&current, "ollama:arbitrary-native-name").unwrap();
    assert!(changed.credential_env.is_none());
    assert!(profile_with_model(&current, "missing-provider:model").is_err());
}

#[test]
fn write_preview_describes_proposed_content_without_fabricating_additions() {
    let preview = proposed_change_preview(
        "write_file",
        &serde_json::json!({"content":"existing replacement\nsecond line"}),
    );
    assert!(preview.contains("拟写入内容（可能覆盖现有文件）"));
    assert!(preview.contains("```text\nexisting replacement\nsecond line\n```"));
    assert!(!preview.contains("```diff"));
    assert!(!preview.contains("+ existing"));
}

#[test]
fn tool_projection_preserves_real_lines_and_failed_shell_status() {
    assert_eq!(
        readable_tool_output(r#"{"text":"第一行\n第二行","bytes":22}"#),
        "第一行\n第二行"
    );
    assert_eq!(
        readable_tool_output(r#"{"exit_code":2,"stdout":"out\n","stderr":"problem"}"#),
        "stderr:\nproblem\nexit: 2\nstdout:\nout\n"
    );
    assert!(
        readable_tool_output(r#"{"error":"cancelled","effect":"unknown"}"#).contains("需要核查")
    );
}

fn local_app() -> (tempfile::TempDir, Engine, App) {
    let dir = tempfile::tempdir().unwrap();
    let profile = Profile::from_model("openai:test-model").unwrap();
    let engine = Engine::open(
        &dir.path().join("data"),
        dir.path(),
        None,
        profile.clone(),
        "test".into(),
        Default::default(),
    )
    .unwrap();
    let app = App::new(Settings {
        profile_name: "test".into(),
        profile,
        profiles: vec![],
    });
    (dir, engine, app)
}

#[test]
fn older_pages_advance_through_internal_only_records() {
    let (dir, mut engine, mut app) = local_app();
    let first = engine.post_message("EARLIEST_PUBLIC_MESSAGE").unwrap();
    let data = dir.path().join("data");
    let connection = rusqlite::Connection::open(data.join("sessions.sqlite3")).unwrap();
    for _ in 0..70 {
        let event = Event::new(&engine.state().id, "audit_marker", serde_json::json!({}));
        connection
            .execute(
                "INSERT INTO events (id,session_id,revision,payload) VALUES (?1,?2,?3,?4)",
                rusqlite::params![
                    event.id,
                    event.session_id,
                    0,
                    serde_json::to_string(&event).unwrap()
                ],
            )
            .unwrap();
    }
    engine.post_message("LATEST_PUBLIC_MESSAGE").unwrap();
    app.load(&mut engine, &data).unwrap();
    assert!(
        !app.ui
            .messages
            .iter()
            .any(|m| m.event_id.as_deref() == Some(&first))
    );
    let before = app.older.clone();
    app.load_older(&engine, &data).unwrap();
    assert_ne!(
        app.older, before,
        "a page with no visible messages must still move the durable cursor"
    );
    app.load_older(&engine, &data).unwrap();
    assert!(
        app.ui
            .messages
            .iter()
            .any(|m| m.event_id.as_deref() == Some(&first))
    );
}

#[test]
fn obsolete_question_keeps_its_draft_and_unknown_result_never_claims_completion() {
    let (_dir, mut engine, mut app) = local_app();
    app.reply_target = Some("question-no-longer-open".into());
    app.ui.paste("draft for the original question");
    app.metadata(&engine);
    assert_eq!(app.reply_target.as_deref(), Some("question-no-longer-open"));
    assert!(app.ui.reply_label.contains("失效"));
    assert!(
        engine
            .post(&app.ui.draft(), app.reply_target.as_deref())
            .is_err()
    );
    assert_eq!(app.ui.draft(), "draft for the original question");
    let mut event = Event::new(
        &engine.state().id,
        "tool_result",
        serde_json::json!({
            "tool_name":"shell", "uncertain":true,
            "message":rig_core::completion::Message::user(r#"{"effect":"unknown"}"#)
        }),
    );
    event.call_id = Some("interrupted-write".into());
    ingest(&engine, &event, &mut app.ui).unwrap();
    let card = app
        .ui
        .messages
        .iter()
        .find(|m| m.event_id.as_deref() == Some("tool:interrupted-write"))
        .unwrap();
    assert!(card.text.starts_with("结果未知"));
    assert!(!card.text.starts_with("完成"));
}

#[test]
fn admission_notice_follows_exact_input_identity_across_shared_request_roots() {
    let (_dir, mut engine, mut app) = local_app();
    let id = engine.post_message("new requirement").unwrap();
    app.ingest(&engine, &engine.read_event(&id).unwrap())
        .unwrap();
    let mut admitted = Event::new(
        &engine.state().id,
        "input_handled",
        serde_json::json!({"input_id":id}),
    );
    admitted.root_input = Some("original-question-root".into());
    app.ingest(&engine, &admitted).unwrap();
    assert_eq!(app.ui.notice, format!("输入 {} 已纳入执行", short_id(&id)));
    let receipt = app
        .ui
        .messages
        .iter()
        .find(|m| m.event_id.as_deref() == Some(id.as_str()))
        .unwrap();
    assert!(receipt.role.contains("已纳入"));
    assert_eq!(receipt.text, "new requirement");
}

#[test]
fn saved_answer_draft_keeps_its_explicit_target_after_reload() {
    let (dir, mut engine, mut app) = local_app();
    app.reply_target = Some("question-original-target".into());
    app.ui.paste("unfinished answer");
    app.save(&engine, &dir.path().join("data")).unwrap();
    let mut reopened = App::new(app.settings);
    reopened
        .load(&mut engine, &dir.path().join("data"))
        .unwrap();
    assert_eq!(reopened.ui.draft(), "unfinished answer");
    assert_eq!(
        reopened.reply_target.as_deref(),
        Some("question-original-target")
    );
    assert!(reopened.ui.reply_label.contains("失效"));
    assert!(
        engine
            .post(&reopened.ui.draft(), reopened.reply_target.as_deref())
            .is_err()
    );
}

#[test]
fn temporary_reconciliation_cancel_and_save_preserve_original_draft_and_target() {
    let (dir, engine, mut app) = local_app();
    app.reply_target = Some("explicit-question".into());
    app.ui.paste("line one\nline two");
    app.ui.handle_key(crossterm::event::KeyEvent::new(
        KeyCode::Home,
        KeyModifiers::NONE,
    ));
    app.ui.handle_key(crossterm::event::KeyEvent::new(
        KeyCode::Right,
        KeyModifiers::SHIFT,
    ));
    let cursor = app.ui.cursor();
    let selection = app.ui.selected_input_text();
    app.ui.open_detail("original evidence", "reader content");
    app.ui.begin_temporary_draft();
    app.reconcile_call = Some("unknown-call".into());
    app.ui.paste("inspect actual effects before recording");
    app.save(&engine, &dir.path().join("data")).unwrap();
    let saved = services::load(&dir.path().join("data"), &engine.state().id).unwrap();
    assert_eq!(saved.draft, "line one\nline two");
    assert_eq!(saved.reply_to.as_deref(), Some("explicit-question"));
    app.cancel_reconcile();
    assert_eq!(app.ui.draft(), "line one\nline two");
    assert_eq!(app.ui.cursor(), cursor);
    assert_eq!(app.reply_target.as_deref(), Some("explicit-question"));
    assert_eq!(
        app.ui.detail.as_ref().map(|(title, _)| title.as_str()),
        Some("original evidence")
    );
    app.ui.close_layer();
    assert_eq!(app.ui.selected_input_text(), selection);
}

#[test]
fn explicit_target_switch_owns_separate_message_and_question_drafts() {
    let (_dir, engine, mut app) = local_app();
    app.ui.paste("original message draft");
    app.ui.handle_key(crossterm::event::KeyEvent::new(
        KeyCode::Left,
        KeyModifiers::SHIFT,
    ));
    let cursor = app.ui.cursor();
    let selection = app.ui.selected_input_text();
    app.switch_target(Some("question-a".into()));
    assert_eq!(app.ui.draft(), "");
    app.ui.paste("answer draft a");
    app.switch_target(Some("question-b".into()));
    app.ui.paste("answer draft b");
    app.cancel_reply();
    assert_eq!(app.ui.draft(), "original message draft");
    assert_eq!(app.ui.cursor(), cursor);
    assert_eq!(app.ui.selected_input_text(), selection);
    app.switch_target(Some("question-a".into()));
    assert_eq!(app.ui.draft(), "answer draft a");
    assert!(app.bind_reply(&engine, "expired-other-question").is_err());
    assert_eq!(app.reply_target.as_deref(), Some("question-a"));
    assert_eq!(app.ui.draft(), "answer draft a");
}

#[test]
fn pause_with_input_selection_keeps_draft_and_always_pauses_engine() {
    let (_dir, mut engine, mut app) = local_app();
    engine.post_message("pending request").unwrap();
    app.ui.paste("preserve selected draft");
    app.ui.handle_key(crossterm::event::KeyEvent::new(
        KeyCode::Left,
        KeyModifiers::SHIFT,
    ));
    let selection = app.ui.selected_input_text();
    pause(&mut engine, &mut app.ui).unwrap();
    assert!(engine.state().paused);
    assert_eq!(app.ui.draft(), "preserve selected draft");
    assert_eq!(app.ui.selected_input_text(), selection);
}

#[test]
fn exact_completed_command_has_no_inline_layer_and_confirmation_only_inserts() {
    let (dir, engine, mut app) = local_app();
    app.ui.paste("/status");
    app.refresh_completion(&engine, &dir.path().join("data"))
        .unwrap();
    assert!(!app.ui.is_completion());
    app.ui.take_draft();
    app.ui.paste("/sta");
    app.refresh_completion(&engine, &dir.path().join("data"))
        .unwrap();
    assert!(app.ui.is_completion());
    app.complete_inline(PickerKind::Command, "/status");
    assert_eq!(app.ui.draft(), "/status ");
    assert!(app.ui.detail.is_none());
    assert!(!app.ui.is_completion());
}

#[test]
fn failed_tool_keeps_reason_first_and_mechanical_success_summary_uses_observed_facts() {
    let (_dir, engine, mut app) = local_app();
    let mut failed = Event::new(
        &engine.state().id,
        "tool_result",
        serde_json::json!({
            "tool_name":"shell", "message":rig_core::completion::Message::user(r#"{"exit_code":2,"stdout":"ordinary output","stderr":"actual failure reason\nsecond detail"}"#)
        }),
    );
    failed.call_id = Some("failed-call".into());
    app.ingest(&engine, &failed).unwrap();
    let card = app
        .ui
        .messages
        .iter()
        .find(|m| m.event_id.as_deref() == Some("tool:failed-call"))
        .unwrap();
    assert!(card.text.starts_with("失败\nactual failure reason"));
    assert!(
        card.summary
            .as_deref()
            .unwrap()
            .starts_with("失败 · exit 2 · actual failure reason")
    );
    assert!(card.text.contains("ordinary output"));
    assert!(
        app.current_status(&engine)
            .contains("最近工具失败 shell failed-c")
    );
    app.ui.notice = "short operation notice".into();
    app.metadata(&engine);
    app.notice_until = Some(Instant::now());
    assert!(app.expire_notice());
    assert!(app.ui.notice.is_empty());
    assert!(app.current_status(&engine).contains("最近工具失败"));
    let mut success = Event::new(
        &engine.state().id,
        "tool_result",
        serde_json::json!({
            "tool_name":"shell", "message":rig_core::completion::Message::user(r#"{"exit_code":0,"stdout":"one\ntwo\n","stderr":""}"#)
        }),
    );
    success.call_id = Some("successful-call".into());
    app.ingest(&engine, &success).unwrap();
    let card = app
        .ui
        .messages
        .iter()
        .find(|m| m.event_id.as_deref() == Some("tool:successful-call"))
        .unwrap();
    assert!(
        card.summary
            .as_deref()
            .unwrap()
            .contains("exit 0 · stdout 2 行 · stderr 0 行")
    );
    assert!(
        app.current_status(&engine).contains("failed-c"),
        "unrelated success must not erase the failed call fact"
    );
}

#[test]
fn result_content_precedes_a_separate_audit_layer() {
    let (_dir, engine, _app) = local_app();
    let mut event = Event::new(
        &engine.state().id,
        "tool_result",
        serde_json::json!({
            "tool_name":"shell", "message":rig_core::completion::Message::user(r#"{"exit_code":0,"stdout":"complete result content","stderr":""}"#)
        }),
    );
    event.call_id = Some("durable-call".into());
    let content = detail(&engine, &event).unwrap();
    assert!(content.starts_with("exit: 0\ncomplete result content"));
    assert!(!content.contains("timestamp (Unix ms)"));
    let audit = audit_detail(&event).unwrap();
    assert!(audit.contains("call: durable-call"));
    assert!(audit.contains("root_input:"));
    assert!(audit.contains("complete result content"));
}

#[test]
fn arriving_question_never_binds_an_empty_or_existing_message_draft() {
    let (dir, mut engine, mut app) = local_app();
    let input = engine.post_message("public requirement").unwrap();
    let id = engine.state().id.clone();
    let job = engine.state().focus.clone().unwrap();
    let mut question = Event::new(
        &id,
        "question",
        serde_json::json!({"question":"Choose an option", "tool_key":"synthetic-question-key"}),
    );
    question.job_id = Some(job);
    question.reply_to = Some(input.clone());
    question.root_input = Some(input);
    let data = dir.path().join("data");
    let connection = rusqlite::Connection::open(data.join("sessions.sqlite3")).unwrap();
    connection
        .execute(
            "INSERT INTO events (id,session_id,revision,payload) VALUES (?1,?2,?3,?4)",
            rusqlite::params![
                question.id,
                id,
                0,
                serde_json::to_string(&question).unwrap()
            ],
        )
        .unwrap();
    drop(engine);
    let engine = Engine::open(
        &data,
        dir.path(),
        Some(&id),
        app.settings.profile.clone(),
        "test".into(),
        Default::default(),
    )
    .unwrap();
    assert_eq!(engine.unanswered_questions().len(), 1);
    app.metadata(&engine);
    assert!(app.reply_target.is_none());
    app.ui.paste("unsent original request");
    app.ingest(&engine, &question).unwrap();
    app.metadata(&engine);
    assert!(app.reply_target.is_none());
    assert_eq!(app.ui.draft(), "unsent original request");
    assert!(app.ui.notice.contains("显式选择"));
    app.bind_reply(&engine, &question.id).unwrap();
    app.metadata(&engine);
    assert!(app.ui.reply_label.contains("Choose an option"));
    assert!(app.ui.reply_label.contains(&short_id(&question.id)));
    assert_eq!(app.reply_target.as_deref(), Some(question.id.as_str()));
    assert_eq!(app.ui.draft(), "");
    app.ui.paste("separate answer");
    app.cancel_reply();
    assert_eq!(app.ui.draft(), "unsent original request");
}

#[test]
fn latest_history_reload_preserves_all_target_drafts_and_rejects_active_reconciliation() {
    let (dir, mut engine, mut app) = local_app();
    let data = dir.path().join("data");
    app.ui.paste("original request\nsecond line");
    app.ui.handle_key(crossterm::event::KeyEvent::new(
        KeyCode::Left,
        KeyModifiers::SHIFT,
    ));
    let message_cursor = app.ui.cursor();
    let message_selection = app.ui.selected_input_text();
    app.switch_target(Some("explicit-question-target".into()));
    app.ui.paste("unfinished answer\nanswer line two");
    app.ui.handle_key(crossterm::event::KeyEvent::new(
        KeyCode::Home,
        KeyModifiers::NONE,
    ));
    app.ui.handle_key(crossterm::event::KeyEvent::new(
        KeyCode::Right,
        KeyModifiers::SHIFT,
    ));
    let answer_cursor = app.ui.cursor();
    let answer_selection = app.ui.selected_input_text();
    let newest = engine
        .post_message("newest durable transcript record")
        .unwrap();
    app.refresh_latest(&mut engine, &data).unwrap();
    assert!(
        app.ui
            .messages
            .iter()
            .any(|m| m.event_id.as_deref() == Some(newest.as_str()))
    );
    assert_eq!(
        app.reply_target.as_deref(),
        Some("explicit-question-target")
    );
    assert_eq!(app.ui.draft(), "unfinished answer\nanswer line two");
    assert_eq!(app.ui.cursor(), answer_cursor);
    assert_eq!(app.ui.selected_input_text(), answer_selection);
    app.cancel_reply();
    assert_eq!(app.ui.draft(), "original request\nsecond line");
    assert_eq!(app.ui.cursor(), message_cursor);
    assert_eq!(app.ui.selected_input_text(), message_selection);
    app.switch_target(Some("explicit-question-target".into()));
    assert_eq!(app.ui.draft(), "unfinished answer\nanswer line two");
    assert_eq!(app.ui.cursor(), answer_cursor);
    assert_eq!(app.ui.selected_input_text(), answer_selection);
    app.ui.begin_temporary_draft();
    app.reconcile_call = Some("unknown-call".into());
    app.ui.paste("temporary inspection note");
    assert!(app.refresh_latest(&mut engine, &data).is_err());
    assert_eq!(app.reconcile_call.as_deref(), Some("unknown-call"));
    assert_eq!(app.ui.draft(), "temporary inspection note");
    assert_eq!(
        app.ui.temporary_draft_text().as_deref(),
        Some("unfinished answer\nanswer line two")
    );
    app.cancel_reconcile();
    assert_eq!(app.ui.draft(), "unfinished answer\nanswer line two");
    assert_eq!(app.ui.cursor(), answer_cursor);
    assert_eq!(app.ui.selected_input_text(), answer_selection);
    assert_eq!(
        app.reply_target.as_deref(),
        Some("explicit-question-target")
    );
}

#[test]
fn failed_shell_summary_prefers_failure_markers_and_labels_unmarked_tail_as_observed() {
    let (_dir, engine, mut app) = local_app();
    let output = serde_json::json!({"exit_code":17,"stdout":"", "stderr":"test_boolean_expiry ... ok\nFAIL: test_boundary (BoundaryTests)\nFAILED (failures=1)"});
    let mut event = Event::new(
        &engine.state().id,
        "tool_result",
        serde_json::json!({"tool_name":"shell", "message":rig_core::completion::Message::user(output.to_string())}),
    );
    event.call_id = Some("failure-marked".into());
    app.ingest(&engine, &event).unwrap();
    let card = app
        .ui
        .messages
        .iter()
        .find(|m| m.event_id.as_deref() == Some("tool:failure-marked"))
        .unwrap();
    assert_eq!(
        card.summary.as_deref(),
        Some("失败 · exit 17 · FAIL: test_boundary (BoundaryTests)")
    );
    assert!(card.text.contains("test_boolean_expiry ... ok"));
    let unmarked = serde_json::json!({"exit_code":17,"stdout":"", "stderr":"first informational line\nlast observed line"});
    event.data["message"] =
        serde_json::to_value(rig_core::completion::Message::user(unmarked.to_string())).unwrap();
    let (exit, observation) =
        tool_failure_observation(&engine, &event, &event_body(&engine, &event).unwrap()).unwrap();
    assert_eq!(exit, Some(17));
    assert_eq!(observation, "stderr尾部观察：last observed line");
    assert_eq!(question_summary("多字节问题内容需要截断", 8), "多字节问…");
}

#[test]
fn reconcile_editor_keys_do_not_cancel_or_submit_from_audit_or_nested_reader() {
    let (_dir, mut engine, mut app) = local_app();
    let input = engine
        .post_message("durable record for the audit reader")
        .unwrap();
    app.ingest(&engine, &engine.read_event(&input).unwrap())
        .unwrap();
    pause(&mut engine, &mut app.ui).unwrap();
    app.ui.paste("original draft");
    app.ui.begin_temporary_draft();
    app.reconcile_call = Some("unknown-call".into());
    app.ui
        .paste("inspection note that must survive audit navigation");
    let esc = crossterm::event::KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    let enter = crossterm::event::KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    let audit = crossterm::event::KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE);
    app.ui.handle_key(audit);
    assert!(app.ui.show_activity);
    assert!(!app.handle_reconcile_key(&mut engine, esc).unwrap());
    app.ui.handle_key(esc);
    assert_eq!(app.reconcile_call.as_deref(), Some("unknown-call"));
    assert_eq!(
        app.ui.draft(),
        "inspection note that must survive audit navigation"
    );
    app.ui.handle_key(audit);
    assert!(!app.handle_reconcile_key(&mut engine, enter).unwrap());
    let record = engine.read_event(app.ui.selected_event().unwrap()).unwrap();
    app.ui
        .open_detail("audit original", detail(&engine, &record).unwrap());
    assert!(!app.handle_reconcile_key(&mut engine, esc).unwrap());
    app.ui.handle_key(esc);
    assert!(app.ui.show_activity);
    assert!(!app.handle_reconcile_key(&mut engine, esc).unwrap());
    app.ui.handle_key(esc);
    assert!(app.reconcile_editor_active());
    assert_eq!(
        app.ui.draft(),
        "inspection note that must survive audit navigation"
    );
    assert!(engine.state().paused);
    assert!(app.handle_reconcile_key(&mut engine, esc).unwrap());
    assert_eq!(app.ui.draft(), "original draft");
    assert!(app.reconcile_call.is_none());
    app.ui.handle_key(audit);
    app.ui.open_detail("nested audit evidence", "read only");
    app.switch_target(Some("explicit-answer-target".into()));
    assert!(!app.ui.show_activity);
    assert!(!app.ui.has_modal());
    assert_eq!(app.ui.focus, Focus::Input);
    app.cancel_reply();
    assert_eq!(app.ui.draft(), "original draft");
}
