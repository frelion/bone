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
        "exit: 2\nout\n\nstderr:\nproblem"
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
    assert_eq!(app.ui.notice, "新要求已纳入执行");
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
