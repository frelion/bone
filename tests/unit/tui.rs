use super::*;

#[test]
fn changing_a_model_keeps_endpoint_and_does_not_move_credentials_between_providers() {
    let current: Profile = serde_json::from_value(serde_json::json!({
        "model":{"model":"old","config":{"openai":{"api_key":"","base_url":"http://127.0.0.1:1234/v1","dialect":"openai","route":"Responses","auth":"Bearer"}}},
        "credential_env":"LOCAL_FIXTURE_KEY","max_tokens":4000
    })).unwrap();
    let next = current.with_model("openai:new-model").unwrap();
    let encoded = serde_json::to_value(&next).unwrap();
    assert_eq!(
        encoded["model"]["config"]["openai"]["base_url"],
        "http://127.0.0.1:1234/v1"
    );
    assert_eq!(encoded["model"]["model"], "new-model");
    assert_eq!(next.credential_env, current.credential_env);
    let changed = current.with_model("ollama:arbitrary-native-name").unwrap();
    assert!(changed.credential_env.is_none());
    assert!(current.with_model("missing-provider:model").is_err());
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
    });
    (dir, engine, app)
}

#[test]
fn session_title_uses_first_real_input_after_initial_stop_and_keeps_narrow_state_visible() {
    let (dir, mut engine, _app) = local_app();
    let data = dir.path().join("data");
    engine.stop().unwrap();
    engine.post_message("TITLE_FROM_FIRST_REAL_INPUT").unwrap();
    let events = engine.events().unwrap();
    assert_eq!(events.first().unwrap().kind, "stopped");
    let items = session_items(&data, &engine.state().workspace, &engine.state().id).unwrap();
    let current = items
        .iter()
        .find(|item| item.value == engine.state().id)
        .unwrap();
    assert!(current.label.contains("TITLE_FROM_FIRST_REAL_INPUT"));
    assert!(!current.label.contains("新对话"));
    assert_eq!(current.value, engine.state().id);
    assert!(!current.detail.contains(&engine.state().id));
    let status_end = current.detail.find("运行").unwrap() + "运行".len();
    assert!(unicode_width::UnicodeWidthStr::width(&current.detail[..status_end]) <= 20);
    assert_eq!(engine.events().unwrap().len(), events.len());
    assert!(
        events
            .iter()
            .all(|event| !matches!(event.kind.as_str(), "model_started" | "tool_started"))
    );
}

fn replace_connection_field(app: &mut App, text: &str) {
    app.ui.handle_key(crossterm::event::KeyEvent::new(
        KeyCode::Char('a'),
        KeyModifiers::CONTROL,
    ));
    app.ui.handle_key(crossterm::event::KeyEvent::new(
        KeyCode::Char('k'),
        KeyModifiers::CONTROL,
    ));
    app.ui.handle_paste(text);
}

#[test]
fn model_form_conflict_preserves_connection_parent_draft_and_external_config() {
    let (dir, mut engine, mut app) = local_app();
    let data = dir.path().join("data");
    let (mut config, revision) = Config::load_with_revision(&data).unwrap();
    config.profiles.insert(
        app.settings.profile_name.clone(),
        app.settings.profile.clone(),
    );
    config.default_profile = app.settings.profile_name.clone();
    config.save_checked(&data, &revision).unwrap();
    app.reply_target = Some("explicit-original-target".into());
    app.ui.paste("original parent draft\nsecond line");
    app.ui.handle_key(crossterm::event::KeyEvent::new(
        KeyCode::Left,
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ));
    let cursor = app.ui.cursor();
    let selection = app.ui.selected_input_text();
    let old_profile = serde_json::to_value(&app.settings.profile).unwrap();
    let old_name = app.settings.profile_name.clone();
    let old_session = engine.state().id.clone();
    app.model_form(&data).unwrap();
    replace_connection_field(&mut app, "new-model-not-in-a-list");

    // A second editor commits after this form captured its exact disk revision.
    let (mut external, external_revision) = Config::load_with_revision(&data).unwrap();
    external.profiles.insert(
        "external".into(),
        Profile::from_model("ollama:external-model").unwrap(),
    );
    external.default_profile = "external".into();
    external.save_checked(&data, &external_revision).unwrap();
    let external_bytes = std::fs::read(data.join("config.toml")).unwrap();
    let error = app.submit_connection_form(&mut engine, &data).unwrap_err();
    assert!(format!("{error:#}").contains("configuration changed"));
    assert_eq!(
        std::fs::read(data.join("config.toml")).unwrap(),
        external_bytes
    );
    assert_eq!(app.settings.profile_name, old_name);
    assert_eq!(
        serde_json::to_value(&app.settings.profile).unwrap(),
        old_profile
    );
    assert_eq!(engine.state().id, old_session);
    assert!(engine.state().jobs.is_empty());
    assert!(
        engine
            .events()
            .unwrap()
            .iter()
            .all(|event| !matches!(event.kind.as_str(), "model_started" | "tool_started"))
    );
    assert!(app.ui.form_is_open());
    assert!(app.connection_setup.is_some());
    assert_eq!(
        app.ui.form_values().unwrap(),
        vec!["new-model-not-in-a-list"]
    );
    assert_eq!(app.ui.draft(), "original parent draft\nsecond line");
    assert_eq!(
        app.reply_target.as_deref(),
        Some("explicit-original-target")
    );
    app.ui.close_layer();
    assert_eq!(app.ui.cursor(), cursor);
    assert_eq!(app.ui.selected_input_text(), selection);
}

#[test]
fn api_final_submit_revalidates_skipped_name_and_cannot_overwrite_competing_key() {
    let (dir, mut engine, mut app) = local_app();
    let data = dir.path().join("data");
    app.ui.paste("parent draft stays untouched");
    let old_profile = serde_json::to_value(&app.settings.profile).unwrap();
    let before_events = engine.events().unwrap().len();
    app.connection_choice(&mut engine, &data, "@api/openai")
        .unwrap();
    replace_connection_field(&mut app, "../outside");
    // Tab skips per-field Enter validation, so final submit must validate again.
    for _ in 0..3 {
        app.ui.handle_key(crossterm::event::KeyEvent::new(
            KeyCode::Tab,
            KeyModifiers::NONE,
        ));
    }
    app.ui.handle_paste("synthetic-unsaved-key");
    assert_eq!(app.ui.form_step(), Some((3, 4)));
    assert!(app.submit_connection_form(&mut engine, &data).is_err());
    assert!(!dir.path().join("outside").exists());
    assert!(app.ui.form_is_open());
    for _ in 0..3 {
        app.ui.handle_key(crossterm::event::KeyEvent::new(
            KeyCode::BackTab,
            KeyModifiers::SHIFT,
        ));
    }
    replace_connection_field(&mut app, "competing");
    for _ in 0..3 {
        app.ui.handle_key(crossterm::event::KeyEvent::new(
            KeyCode::Tab,
            KeyModifiers::NONE,
        ));
    }
    let competitor_profile = Profile::from_model("openai:competitor-model").unwrap();
    std::fs::create_dir_all(data.join("profiles")).unwrap();
    std::fs::create_dir(data.join("profiles/competing")).unwrap();
    bone::save_api_key(
        &data,
        "competing",
        &competitor_profile,
        "synthetic-existing-key",
    )
    .unwrap();
    let key_path = data.join("profiles/competing/api-key");
    let original_key_bytes = std::fs::read(&key_path).unwrap();
    let error = app.submit_connection_form(&mut engine, &data).unwrap_err();
    assert!(format!("{error:#}").contains("连接名称已被使用"));
    assert_eq!(std::fs::read(key_path).unwrap(), original_key_bytes);
    assert!(!data.join("config.toml").exists());
    assert_eq!(engine.events().unwrap().len(), before_events);
    assert!(engine.state().jobs.is_empty());
    assert_eq!(
        serde_json::to_value(&app.settings.profile).unwrap(),
        old_profile
    );
    assert_eq!(app.settings.profile_name, "test");
    assert!(app.ui.form_is_open());
    assert_eq!(app.ui.form_values().unwrap()[3], "synthetic-unsaved-key");
    app.ui.close_layer();
    assert_eq!(app.ui.draft(), "parent draft stays untouched");
}

#[test]
fn sidebar_session_switch_during_reconciliation_keeps_note_target_and_original_draft() {
    let (dir, mut engine, mut app) = local_app();
    let data = dir.path().join("data");
    let other = Engine::open(
        &data,
        dir.path(),
        None,
        app.settings.profile.clone(),
        "test".into(),
        Default::default(),
    )
    .unwrap();
    let other_id = other.state().id.clone();
    drop(other);
    app.reply_target = Some("original-question-target".into());
    app.ui.paste("original answer draft\noriginal second line");
    app.ui.handle_key(crossterm::event::KeyEvent::new(
        KeyCode::Left,
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ));
    let cursor = app.ui.cursor();
    let selection = app.ui.selected_input_text();
    app.ui.begin_temporary_draft();
    app.reconcile_call = Some("unknown-call-being-reviewed".into());
    app.ui.paste("unfinished observed reconciliation note");
    app.ui.focus = Focus::Sessions;
    let original_session = engine.state().id.clone();
    let before_events = engine.events().unwrap().len();
    assert!(
        app.switch_session(&mut engine, &data, Some(&other_id))
            .is_err()
    );
    assert_eq!(engine.state().id, original_session);
    assert_eq!(engine.events().unwrap().len(), before_events);
    assert_eq!(app.ui.focus, Focus::Sessions);
    assert_eq!(
        app.reconcile_call.as_deref(),
        Some("unknown-call-being-reviewed")
    );
    assert_eq!(
        app.reply_target.as_deref(),
        Some("original-question-target")
    );
    assert_eq!(app.ui.draft(), "unfinished observed reconciliation note");
    app.cancel_reconcile();
    assert_eq!(
        app.ui.draft(),
        "original answer draft\noriginal second line"
    );
    assert_eq!(app.ui.cursor(), cursor);
    assert_eq!(app.ui.selected_input_text(), selection);
    assert_eq!(
        app.reply_target.as_deref(),
        Some("original-question-target")
    );
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
    assert_eq!(app.ui.notice, "新要求已纳入执行");
    assert_eq!(app.last_admitted_input.as_deref(), Some(id.as_str()));
    let receipt = app
        .ui
        .messages
        .iter()
        .find(|m| m.event_id.as_deref() == Some(id.as_str()))
        .unwrap();
    assert_eq!(receipt.kind, MessageKind::User { admitted: true });
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
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
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
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
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
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
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
    app.ui.paste("/he");
    app.refresh_completion(&engine, &dir.path().join("data"))
        .unwrap();
    assert!(app.ui.is_completion());
    app.complete_inline(PickerKind::Command, "/help");
    assert_eq!(app.ui.draft(), "/help ");
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
    app.ui.notify("short operation notice");
    app.metadata(&engine);
    app.ui.notice_until = Some(std::time::Instant::now());
    assert!(app.ui.expire_notice());
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
    assert!(app.ui.notice.contains("选择回复"));
    app.bind_reply(&engine, &question.id).unwrap();
    app.metadata(&engine);
    assert!(app.ui.reply_label.contains("Choose an option"));
    assert!(!app.ui.reply_label.contains(&short_id(&question.id)));
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
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
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
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
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

#[path = "../support/server.rs"]
mod feedback_server;

fn execution_feedback_fixture(
    turns: serde_json::Value,
) -> (tempfile::TempDir, Engine, App, feedback_server::Server) {
    use rig_core::providers::{
        openai::{OpenAIConfig, Route},
        registry::{ProviderConfig, ProviderRef},
    };
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("responses.json");
    let requests = dir.path().join("requests.jsonl");
    std::fs::write(
        &script,
        serde_json::to_vec(&serde_json::json!({"turns":turns})).unwrap(),
    )
    .unwrap();
    let (server, port) =
        feedback_server::Server::script("tests/scripted_responses.py", &script, &requests);
    let mut native = OpenAIConfig::new("")
        .with_base_url(format!("http://127.0.0.1:{port}/v1"))
        .with_route(Route::Responses);
    native.dialect = rig_core::providers::openai::wire::LLAMACPP;
    native.auth = native.dialect.quirks.auth;
    let profile = Profile {
        model: ModelReference::Registry(
            ProviderRef::configured(ProviderConfig::OpenAi(native), "feedback-fixture").unwrap(),
        ),
        credential_env: Some(format!("BONE_TEST_EMPTY_{}", uuid::Uuid::new_v4().simple())),
        reuse_codex_login: false,
        additional_params: None,
        max_tokens: None,
    };
    let engine = Engine::open(
        &dir.path().join("data"),
        dir.path(),
        None,
        profile.clone(),
        "feedback-fixture".into(),
        Default::default(),
    )
    .unwrap();
    let app = App::new(Settings {
        profile_name: "feedback-fixture".into(),
        profile,
    });
    (dir, engine, app, server)
}

#[tokio::test]
async fn an_old_delivery_cannot_hide_a_new_real_model_call_or_restart_its_clock() {
    let (dir, mut engine, mut app, _server) = execution_feedback_fixture(serde_json::json!([
        {"text":"first completed delivery"}, {"delay_seconds":1,"text":"later response"}
    ]));
    let data = dir.path().join("data");
    let first = engine.post_message("first requirement").unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            engine.step().await.unwrap();
            app.sync(&engine, &data).unwrap();
            if engine
                .result(&first)
                .is_some_and(|event| event.kind == "delivery")
            {
                break;
            }
        }
    })
    .await
    .unwrap();
    assert!(app.ui.live_status.contains("已完成"));
    assert!(!app.ui.busy);
    engine
        .post_message("new work after the displayed delivery")
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let events = engine.step().await.unwrap();
            if events.iter().any(|event| event.kind == "model_started") {
                break;
            }
        }
    })
    .await
    .unwrap();
    // Keep the old displayed input association: execution facts must still win.
    assert_eq!(app.last_input.as_deref(), Some(first.as_str()));
    assert_eq!(engine.result(&first).unwrap().kind, "delivery");
    app.metadata(&engine);
    assert!(app.ui.live_status.starts_with("思考中"));
    assert!(app.ui.live_status.contains("s"));
    assert!(!app.ui.live_status.contains("完成"));
    assert!(app.ui.busy);
    let call = engine
        .state()
        .jobs
        .values()
        .find_map(|job| job.current_call.clone())
        .unwrap();
    let started = app.active_calls[&call].started;
    app.metadata(&engine);
    assert_eq!(app.active_calls[&call].started, started);
    pause(&mut engine, &mut app.ui).unwrap();
    app.metadata(&engine);
    assert!(app.ui.live_status.starts_with("已暂停"));
    assert!(!app.ui.busy);
    assert_eq!(app.ui.spinner_tick, 0);
}

#[tokio::test]
async fn a_quiet_real_shell_animates_and_waiting_for_a_reply_does_not() {
    let (dir, mut engine, mut app, _server) = execution_feedback_fixture(serde_json::json!([
        {"output":[{"type":"function_call","call_id":"quiet-shell","name":"shell","arguments":{"command":"sleep 0.6"}}]},
        {"output":[{"type":"function_call","call_id":"ask-next","name":"ask_user","arguments":{"question":"Which file should be read?"}}]}
    ]));
    let data = dir.path().join("data");
    engine
        .post_message("run the local quiet command then ask a question")
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let events = engine.step().await.unwrap();
            app.sync(&engine, &data).unwrap();
            if events
                .iter()
                .any(|event| event.kind == "tool_started" && event.data["tool_name"] == "shell")
            {
                break;
            }
        }
    })
    .await
    .unwrap();
    app.ui.notice.clear();
    app.metadata(&engine);
    assert!(app.ui.live_status.starts_with("执行命令 · sleep 0.6"));
    assert!(app.ui.busy);
    assert!(app.ui.feedback_detail.contains("尚未产生输出"));
    let spinner = app.ui.spinner_tick;
    tokio::time::sleep(Duration::from_millis(160)).await;
    app.metadata(&engine);
    assert!(app.ui.spinner_tick > spinner);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            engine.step().await.unwrap();
            app.sync(&engine, &data).unwrap();
            if !engine.unanswered_questions().is_empty() {
                break;
            }
        }
    })
    .await
    .unwrap();
    app.metadata(&engine);
    assert!(app.ui.live_status.starts_with("等待回复"));
    assert!(!app.ui.busy);
    assert_eq!(app.ui.spinner_tick, 0);
}

#[test]
fn session_titles_skip_blank_lines_and_catalog_keeps_an_old_current_session() {
    let (dir, mut engine, _app) = local_app();
    let data = dir.path().join("data");
    let workspace = engine.state().workspace.clone();
    let connection = rusqlite::Connection::open(data.join("sessions.sqlite3")).unwrap();
    connection.execute_batch("BEGIN IMMEDIATE").unwrap();
    for _ in 0..205 {
        let state = bone::state::SessionState::new(&workspace);
        connection
            .execute(
                "INSERT INTO sessions(id,revision,snapshot) VALUES(?1,0,?2)",
                rusqlite::params![state.id, serde_json::to_string(&state).unwrap()],
            )
            .unwrap();
    }
    connection.execute_batch("COMMIT").unwrap();
    assert!(
        session_items(&data, &workspace, &engine.state().id)
            .unwrap()
            .is_empty()
    );
    engine.stop().unwrap();
    assert!(
        session_items(&data, &workspace, &engine.state().id)
            .unwrap()
            .is_empty(),
        "a stop event does not make an unused session visible"
    );
    engine
        .post_message("\n  \n  中文标题保留真实内容  \n第二段")
        .unwrap();
    let items = session_items(&data, &workspace, &engine.state().id).unwrap();
    assert_eq!(
        items.len(),
        1,
        "empty rows must not consume the catalog limit"
    );
    assert_eq!(items[0].value, engine.state().id);
    assert_eq!(items[0].label, "中文标题保留真实内容");
    assert!(items[0].detail.contains("运行"));
    assert!(!items[0].detail.contains("轮"));
    assert!(!items[0].label.contains("当前"));
    let timestamp = engine.events().unwrap().last().unwrap().timestamp.clone();
    let expected = connection
        .query_row(
            "SELECT strftime('%m/%d %H:%M', ?1 / 1000.0, 'unixepoch', 'localtime')",
            [&timestamp],
            |row| row.get::<_, String>(0),
        )
        .unwrap();
    assert_eq!(items[0].updated_at, Some(expected));
    connection.execute_batch("BEGIN IMMEDIATE").unwrap();
    for index in 0..205 {
        let state = bone::state::SessionState::new(&workspace);
        let event = Event::new(
            &state.id,
            "input",
            serde_json::json!({"source":"user", "message":rig_core::completion::Message::user(format!("历史任务 {index}"))}),
        );
        connection
            .execute(
                "INSERT INTO sessions(id,revision,snapshot) VALUES(?1,0,?2)",
                rusqlite::params![state.id, serde_json::to_string(&state).unwrap()],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO events(id,session_id,revision,payload) VALUES(?1,?2,0,?3)",
                rusqlite::params![
                    event.id,
                    event.session_id,
                    serde_json::to_string(&event).unwrap()
                ],
            )
            .unwrap();
    }
    connection.execute_batch("COMMIT").unwrap();
    let items = session_items(&data, &workspace, &engine.state().id).unwrap();
    assert_eq!(items.len(), 200);
    assert_eq!(items[0].value, engine.state().id);
    assert_eq!(items[0].label, "中文标题保留真实内容");
    assert!(
        items[1..]
            .iter()
            .all(|item| item.label.starts_with("历史任务 "))
    );
}

#[test]
fn session_catalog_keeps_existing_work_without_a_user_title() {
    let (dir, engine, _app) = local_app();
    let data = dir.path().join("data");
    let workspace = &engine.state().workspace;
    let connection = rusqlite::Connection::open(data.join("sessions.sqlite3")).unwrap();
    let mut states = Vec::new();
    for status in [
        JobState::Ready,
        JobState::Waiting,
        JobState::Idle,
        JobState::Closed,
    ] {
        let mut state = bone::state::SessionState::new(workspace);
        let mut job = bone::state::Job::new("已有工作");
        job.state = status;
        state.jobs.insert(job.id.clone(), job);
        states.push(state);
    }
    let mut pending = bone::state::SessionState::new(workspace);
    pending.pending_inputs.push_back("pending-input".into());
    states.push(pending);
    let mut unknown = bone::state::SessionState::new(workspace);
    unknown.unknown_writes.insert(
        "unknown-call".into(),
        bone::state::UnknownWrite {
            call_id: "unknown-call".into(),
            job_id: "unknown-job".into(),
            root_input: None,
            tool_name: "shell".into(),
        },
    );
    let unknown_id = unknown.id.clone();
    states.push(unknown);
    for state in &states {
        connection
            .execute(
                "INSERT INTO sessions(id,revision,snapshot) VALUES(?1,0,?2)",
                rusqlite::params![state.id, serde_json::to_string(state).unwrap()],
            )
            .unwrap();
    }
    let items = session_items(&data, workspace, &engine.state().id).unwrap();
    assert_eq!(items.len(), states.len());
    for state in states {
        let item = items.iter().find(|item| item.value == state.id).unwrap();
        assert_eq!(item.label, "会话");
        assert_eq!(item.updated_at, None);
    }
    assert_eq!(
        items
            .iter()
            .find(|item| item.value == unknown_id)
            .unwrap()
            .detail,
        "核查"
    );
    assert!(
        engine.events().unwrap().is_empty(),
        "catalog reads must not create events"
    );
}

#[test]
fn session_activity_time_comes_from_latest_event_sequence_in_local_time() {
    let (dir, mut engine, _app) = local_app();
    let data = dir.path().join("data");
    engine.post_message("首条输入命名会话").unwrap();
    engine.post_message("第二条输入更新会话").unwrap();
    engine.stop().unwrap();
    let events = engine.events().unwrap();
    assert_eq!(events.len(), 3);
    assert_eq!(events.last().unwrap().kind, "stopped");
    let connection = rusqlite::Connection::open(data.join("sessions.sqlite3")).unwrap();
    // Deliberately let the first two events have later wall-clock times. The
    // latest persisted event is defined by sequence, even if the clock moved.
    let timestamps = ["1906634040000", "2230305960000", "1728029520000"];
    for (event, timestamp) in events.iter().zip(timestamps) {
        connection
            .execute(
                "UPDATE events SET
                   payload = json_set(payload, '$.timestamp', ?1),
                   metadata = json_set(metadata, '$.timestamp', ?1)
                 WHERE id = ?2",
                rusqlite::params![timestamp, event.id],
            )
            .unwrap();
    }
    let local_time = |timestamp: &str| {
        connection
            .query_row(
                "SELECT strftime('%Y/%m/%d', ?1 / 1000.0, 'unixepoch', 'localtime')",
                [timestamp],
                |row| row.get::<_, String>(0),
            )
            .unwrap()
    };
    let items = session_items(&data, &engine.state().workspace, &engine.state().id).unwrap();
    assert_eq!(items[0].label, "首条输入命名会话");
    assert_eq!(
        items[0].updated_at.as_deref(),
        Some(local_time(timestamps[2]).as_str())
    );
    assert!(items[0].updated_at.as_ref().unwrap().starts_with("2024/"));
    assert_ne!(
        items[0].updated_at.as_deref(),
        Some(local_time(timestamps[0]).as_str())
    );
    assert_ne!(
        items[0].updated_at.as_deref(),
        Some(local_time(timestamps[1]).as_str())
    );
    assert_eq!(engine.events().unwrap().len(), events.len());
}

#[test]
fn sidebar_metadata_uses_real_attention_and_pause_facts_without_inventing_rounds() {
    let mut state = bone::state::SessionState::new("workspace");
    let mut job = bone::state::Job::new("dependency wait");
    job.state = JobState::Waiting;
    let id = job.id.clone();
    state.jobs.insert(id.clone(), job);
    assert_eq!(session_status(&state, &[]), "等待");
    let mut question = Event::new(
        &state.id,
        "question",
        serde_json::json!({"tool_key":"question-key"}),
    );
    question.job_id = Some(id.clone());
    assert_eq!(session_status(&state, &[question.clone()]), "回复");
    state.paused = true;
    assert_eq!(session_status(&state, &[question.clone()]), "回复");
    let answered = Event::new(
        &state.id,
        "tool_result",
        serde_json::json!({"tool_key":"question-key"}),
    );
    assert_eq!(
        session_status(&state, &[question.clone(), answered]),
        "暂停"
    );
    state.unknown_writes.insert(
        "write".into(),
        bone::state::UnknownWrite {
            call_id: "write".into(),
            job_id: id.clone(),
            root_input: None,
            tool_name: "shell".into(),
        },
    );
    assert_eq!(session_status(&state, &[question.clone()]), "核查");
    state.unknown_writes.clear();
    state.jobs.get_mut(&id).unwrap().state = JobState::Closed;
    assert_eq!(session_status(&state, &[question]), "");
    state.jobs.get_mut(&id).unwrap().state = JobState::Running;
    assert_eq!(session_status(&state, &[]), "暂停");
}

#[tokio::test]
async fn completed_work_reopens_without_a_spurious_resume_prompt() {
    let (dir, mut engine, mut app, _server) = execution_feedback_fixture(serde_json::json!([
        {"text":"检查已完成"}
    ]));
    let data = dir.path().join("data");
    engine.post_message("检查登录边界").unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !engine.is_quiescent() {
            engine.step().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert!(
        engine
            .state()
            .jobs
            .values()
            .all(|job| job.state == JobState::Idle)
    );
    let original = engine.state().id.clone();
    app.load(&mut engine, &data).unwrap();
    app.switch_session(&mut engine, &data, None).unwrap();
    app.switch_session(&mut engine, &data, Some(&original))
        .unwrap();
    assert!(engine.state().paused); // Core recovery policy is unchanged.
    assert!(!app.ui.notice.contains("Ctrl+R"));
    assert_eq!(app.ui.live_status, "本次已完成");
    assert_eq!(
        session_items(&data, &engine.state().workspace, &original).unwrap()[0].detail,
        ""
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("requests.jsonl"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}

#[tokio::test]
async fn automatic_sidebar_refresh_preserves_the_browsed_session_instead_of_selecting_current() {
    let (dir, mut engine, mut app) = local_app();
    let data = dir.path().join("data");
    engine.post_message("当前任务").unwrap();
    let mut other = Engine::open(
        &data,
        &engine.state().workspace,
        None,
        app.settings.profile.clone(),
        "test".into(),
        Default::default(),
    )
    .unwrap();
    other.post_message("另一任务").unwrap();
    other.stop().unwrap();
    app.ui
        .set_sessions(session_items(&data, &engine.state().workspace, &engine.state().id).unwrap());
    app.ui.select_session(&engine.state().id);
    app.ui.handle_key(crossterm::event::KeyEvent::new(
        KeyCode::Left,
        KeyModifiers::SHIFT,
    ));
    app.ui.handle_key(crossterm::event::KeyEvent::new(
        KeyCode::Down,
        KeyModifiers::NONE,
    ));
    let selected = app.ui.selected_session().unwrap().to_owned();
    assert_eq!(selected, other.state().id);
    engine
        .post_message("Background event updates current activity")
        .unwrap();
    app.refresh_sessions(&engine, &data);
    assert_eq!(app.ui.selected_session(), Some(selected.as_str()));
    tokio::time::timeout(Duration::from_secs(5), async {
        while app.session_index.is_some() {
            app.tasks(&mut engine, &data).await;
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(app.ui.selected_session(), Some(selected.as_str()));
    assert_eq!(app.ui.focus, Focus::Sessions);
    drop(other);
    app.switch_session(&mut engine, &data, Some(&selected))
        .unwrap();
    assert!(engine.state().paused);
    assert!(app.ui.notice.contains("Ctrl+R"));
    assert!(
        engine
            .events()
            .unwrap()
            .iter()
            .all(|event| !matches!(event.kind.as_str(), "model_started" | "tool_started"))
    );
}

#[tokio::test]
async fn new_session_returns_to_input_and_pauses_current_work_instead_of_the_sidebar_candidate() {
    let (dir, mut engine, mut app) = local_app();
    let data = dir.path().join("data");
    engine.post_message("当前执行对象").unwrap();
    let original = engine.state().id.clone();
    let mut other = Engine::open(
        &data,
        &engine.state().workspace,
        None,
        app.settings.profile.clone(),
        "test".into(),
        Default::default(),
    )
    .unwrap();
    other.post_message("只浏览的候选对象").unwrap();
    let candidate = other.state().id.clone();
    app.load(&mut engine, &data).unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while app.session_index.is_some() {
            app.tasks(&mut engine, &data).await;
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    app.ui.handle_key(crossterm::event::KeyEvent::new(
        KeyCode::Left,
        KeyModifiers::SHIFT,
    ));
    app.ui.select_session(&candidate);
    assert_eq!(app.ui.focus, Focus::Sessions);
    assert_eq!(app.ui.selected_session(), Some(candidate.as_str()));
    app.switch_session(&mut engine, &data, None).unwrap();
    assert_eq!(app.ui.focus, Focus::Input);
    assert_ne!(engine.state().id, original);
    assert_ne!(engine.state().id, candidate);
    assert!(!engine.state().paused);
    assert!(engine.events().unwrap().is_empty());
    let connection = rusqlite::Connection::open(data.join("sessions.sqlite3")).unwrap();
    let snapshot = |id: &str| {
        let text: String = connection
            .query_row("SELECT snapshot FROM sessions WHERE id = ?1", [id], |row| {
                row.get(0)
            })
            .unwrap();
        serde_json::from_str::<bone::state::SessionState>(&text).unwrap()
    };
    assert!(snapshot(&original).paused);
    assert!(!snapshot(&candidate).paused);
    let before = session_items(&data, &engine.state().workspace, &engine.state().id).unwrap();
    assert_eq!(before.len(), 2);
    assert!(!before.iter().any(|item| item.value == engine.state().id));
    app.ui.paste("新上下文草稿");
    app.ui.handle_key(crossterm::event::KeyEvent::new(
        KeyCode::Left,
        KeyModifiers::SHIFT,
    ));
    app.ui.handle_key(crossterm::event::KeyEvent::new(
        KeyCode::Right,
        KeyModifiers::SHIFT,
    ));
    assert_eq!(app.ui.focus, Focus::Input);
    assert_eq!(app.ui.draft(), "新上下文草稿");
    engine.post_message("新上下文第一条输入").unwrap();
    let after = session_items(&data, &engine.state().workspace, &engine.state().id).unwrap();
    assert_eq!(after.len(), 3);
    assert_eq!(after[0].value, engine.state().id);
    assert_eq!(after[0].label, "新上下文第一条输入");
    assert!(other.events().unwrap().iter().all(|event| !matches!(
        event.kind.as_str(),
        "stopped" | "model_started" | "tool_started"
    )));
}

#[test]
fn switching_sessions_and_reopening_restores_the_exact_multiline_input_position() {
    let (dir, mut engine, mut app) = local_app();
    let data = dir.path().join("data");
    let original = engine.state().id.clone();
    app.ui.paste("第一行 👩‍💻\n第二行 é 末尾");
    app.ui.handle_key(crossterm::event::KeyEvent::new(
        KeyCode::Left,
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ));
    app.ui.handle_key(crossterm::event::KeyEvent::new(
        KeyCode::Left,
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ));
    let position = app.ui.input_position();
    let text = app.ui.draft();
    let selection = app.ui.selected_input_text();
    assert!(selection.is_some());
    app.ui.handle_key(crossterm::event::KeyEvent::new(
        KeyCode::Up,
        KeyModifiers::SHIFT,
    ));
    app.ui.handle_key(crossterm::event::KeyEvent::new(
        KeyCode::Left,
        KeyModifiers::SHIFT,
    ));
    app.switch_session(&mut engine, &data, None).unwrap();
    app.ui.handle_key(crossterm::event::KeyEvent::new(
        KeyCode::Right,
        KeyModifiers::SHIFT,
    ));
    assert_eq!(app.ui.focus, Focus::Input);
    app.ui.handle_key(crossterm::event::KeyEvent::new(
        KeyCode::Up,
        KeyModifiers::SHIFT,
    ));
    app.ui.handle_key(crossterm::event::KeyEvent::new(
        KeyCode::Left,
        KeyModifiers::SHIFT,
    ));
    app.switch_session(&mut engine, &data, Some(&original))
        .unwrap();
    app.ui.handle_key(crossterm::event::KeyEvent::new(
        KeyCode::Right,
        KeyModifiers::SHIFT,
    ));
    assert_eq!(app.ui.focus, Focus::Conversation);
    assert_eq!(app.ui.draft(), text);
    assert_eq!(app.ui.input_position(), position);
    app.ui.handle_key(crossterm::event::KeyEvent::new(
        KeyCode::Down,
        KeyModifiers::SHIFT,
    ));
    assert_eq!(app.ui.selected_input_text(), selection);
    app.save(&engine, &data).unwrap();
    let mut reopened = App::new(Settings {
        profile_name: "test".into(),
        profile: app.settings.profile.clone(),
    });
    reopened.load(&mut engine, &data).unwrap();
    assert_eq!(reopened.ui.input_position(), position);
    assert_eq!(reopened.ui.selected_input_text(), selection);
}

#[test]
fn all_target_drafts_survive_session_switch_and_restart_with_their_positions() {
    let (dir, mut engine, mut app) = local_app();
    let data = dir.path().join("data");
    let original = engine.state().id.clone();
    app.ui.paste("未发送的新要求 👩‍💻\n保留选择");
    app.ui.handle_key(crossterm::event::KeyEvent::new(
        KeyCode::Left,
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ));
    let original_draft = app.ui.draft_snapshot().saved();
    app.switch_target(Some("question-a".into()));
    app.ui.paste("第一份回答 é");
    let answer_a = app.ui.draft_snapshot().saved();
    app.switch_target(Some("question-b".into()));
    app.ui.paste("第二份回答\n尚未提交");
    let answer_b = app.ui.draft_snapshot().saved();
    app.switch_session(&mut engine, &data, None).unwrap();
    app.switch_session(&mut engine, &data, Some(&original))
        .unwrap();
    let mut reopened = App::new(app.settings);
    reopened.load(&mut engine, &data).unwrap();
    assert_eq!(reopened.reply_target.as_deref(), Some("question-b"));
    assert_eq!(reopened.ui.draft_snapshot().saved(), answer_b);
    reopened.switch_target(Some("question-a".into()));
    assert_eq!(reopened.ui.draft_snapshot().saved(), answer_a);
    reopened.cancel_reply();
    assert_eq!(reopened.ui.draft_snapshot().saved(), original_draft);
    // Completed questions must not fill the bounded draft file with empty slots.
    for index in 0..110 {
        reopened.switch_target(Some(format!("completed-question-{index}")));
    }
    reopened.cancel_reply();
    reopened.save(&engine, &data).unwrap();
    assert_eq!(reopened.ui.draft_snapshot().saved(), original_draft);
    assert!(
        engine
            .events()
            .unwrap()
            .iter()
            .all(|event| !matches!(event.kind.as_str(), "model_started" | "tool_started"))
    );
}

#[test]
fn a_new_async_failure_does_not_inherit_an_expired_success_notice_timer() {
    let (_, _, mut app) = local_app();
    app.ui.notify("旧操作完成");
    app.ui.notice_until = Some(std::time::Instant::now());
    app.ui.fail("导出失败：磁盘已满");
    assert!(!app.ui.expire_notice());
    assert_eq!(app.ui.notice, "导出失败：磁盘已满");
    assert_eq!(app.ui.notice_tone, Tone::Error);
    app.ui.notify("后续操作完成");
    assert!(app.ui.notice_until.is_some());
}

#[tokio::test]
async fn a_completed_export_keeps_the_active_form_and_parent_draft() {
    let (dir, mut engine, mut app) = local_app();
    app.ui.paste("继续修复之前先保留这份草稿");
    app.ui.open_form(
        "连接",
        vec![view::FormField {
            label: "模型".into(),
            value: "local-model".into(),
            secret: false,
        }],
    );
    let values = app.ui.form_values();
    let path = dir.path().join("report.html");
    let result = path.clone();
    app.export = Some(tokio::spawn(async move { Ok(result) }));
    while app.export.is_some() {
        app.tasks(&mut engine, &dir.path().join("data")).await;
        tokio::task::yield_now().await;
    }
    assert!(app.ui.form_is_open());
    assert_eq!(app.ui.form_values(), values);
    assert!(app.ui.detail.is_none());
    assert_eq!(app.exported_report.as_deref(), Some(path.as_path()));
    app.commands(&engine, String::new());
    assert!(
        app.ui
            .picker
            .as_ref()
            .unwrap()
            .items
            .iter()
            .any(|item| item.value == "/export show")
    );
    app.ui.close_layer();
    app.ui.close_layer();
    assert_eq!(app.ui.draft(), "继续修复之前先保留这份草稿");
    assert!(engine.events().unwrap().is_empty());
}
