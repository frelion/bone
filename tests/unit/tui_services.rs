use super::*;
#[test]
fn drafts_roundtrip_and_reject_corruption_and_paths() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(load(dir.path(), "session").unwrap().draft, "");
    let saved = UiSaved {
        draft: "中文 draft".into(),
        history: vec!["one".into()],
        reply_to: Some("question-id".into()),
        cursor: Some(3),
        selection: Some((0, 3)),
        ..Default::default()
    };
    save(dir.path(), "session", &saved).unwrap();
    assert_eq!(load(dir.path(), "session").unwrap().history, saved.history);
    assert_eq!(load(dir.path(), "session").unwrap().cursor, saved.cursor);
    assert_eq!(
        load(dir.path(), "session").unwrap().selection,
        saved.selection
    );
    assert_eq!(
        load(dir.path(), "session").unwrap().reply_to,
        saved.reply_to
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(session_path(dir.path(), "session").unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    fs::write(session_path(dir.path(), "session").unwrap(), "broken").unwrap();
    assert!(load(dir.path(), "session").is_err());
    assert!(save(dir.path(), "../escape", &saved).is_err());
    assert!(
        save(
            dir.path(),
            "session",
            &UiSaved {
                draft: "a".repeat(DRAFT_LIMIT + 1),
                history: vec![],
                reply_to: None,
                ..Default::default()
            }
        )
        .is_err()
    );
}
#[test]
fn html_and_opaque_content_are_safe() {
    assert_eq!(
        escape("</script><>&\"'"),
        "&lt;/script&gt;&lt;&gt;&amp;&quot;&#39;"
    );
    let content = serde_json::json!([
        {"type":"reasoning","content":[{"type":"text","text":"hidden"}]},
        {"type":"encrypted","content":"opaque"},
        {"type":"text","text":"可读正文"},
        {"type":"toolresult","content":[{"type":"text","text":"result"}]}
    ]);
    assert_eq!(text(&content), "可读正文\nresult");
    assert_eq!(native_text(&content), "可读正文\n\nresult");
}
#[tokio::test]
async fn git_inspection_reports_untracked_paths_and_non_repositories() {
    let dir = tempfile::tempdir().unwrap();
    assert!(git_diff(dir.path()).await.is_err());
    let status = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(dir.path())
        .status()
        .unwrap();
    assert!(status.success());
    fs::write(dir.path().join("untracked.txt"), "unchanged by inspection").unwrap();
    let output = git_diff(dir.path()).await.unwrap();
    assert!(output.contains("?? untracked.txt"));
    assert!(output.contains("+unchanged by inspection"));
    assert!(output.contains("No newline at end of file"));
    assert_eq!(
        fs::read_to_string(dir.path().join("untracked.txt")).unwrap(),
        "unchanged by inspection"
    );
}
#[tokio::test]
async fn export_reads_durable_conversation_and_escapes_script_markup() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let profile = bone::config::Profile::from_model("openai:gpt-4o-mini").unwrap();
    let mut engine = bone::runtime::Engine::open(
        &data,
        dir.path(),
        None,
        profile,
        "test".into(),
        Default::default(),
    )
    .unwrap();
    let input = engine
        .post("hello </script><script>alert(1)</script>", None)
        .unwrap();
    let response = bone::state::Event::new(
        &engine.state().id,
        "model_message",
        serde_json::json!({
            "response":{"choice":[{"type":"reasoning","text":"hidden reasoning"},{"type":"text","text":"delivered <answer>"}]}
        }),
    );
    let mut delivery = bone::state::Event::new(
        &engine.state().id,
        "delivery",
        serde_json::json!({"response_event":response.id}),
    );
    delivery.reply_to = Some(input);
    let mut connection = rusqlite::Connection::open(data.join("sessions.sqlite3")).unwrap();
    let input_event = engine
        .read_event(delivery.reply_to.as_deref().unwrap())
        .unwrap();
    connection
        .execute("DELETE FROM events WHERE id = ?1", [&input_event.id])
        .unwrap();
    let transaction = connection.transaction().unwrap();
    // Reference order inside one commit is not constrained by append order.
    for event in [delivery, response, input_event] {
        transaction
            .execute(
                "INSERT INTO events (id, session_id, revision, payload) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![
                    event.id,
                    event.session_id,
                    event.revision as i64,
                    serde_json::to_string(&event).unwrap()
                ],
            )
            .unwrap();
    }
    transaction.commit().unwrap();
    let delivered = latest_delivery(&data, &engine.state().id).unwrap().unwrap();
    for _ in 0..300 {
        let event = bone::state::Event::new(
            &engine.state().id,
            "audit_marker",
            serde_json::json!({"note":"delivery is outside the UI window"}),
        );
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
    assert_eq!(
        latest_delivery(&data, &engine.state().id)
            .unwrap()
            .unwrap()
            .id,
        delivered.id,
    );
    assert!(latest_delivery(&data, "another-session").unwrap().is_none());
    let path = export(&data, &engine.state().id).unwrap();
    let html = fs::read_to_string(path).unwrap();
    assert!(html.contains("hello &lt;/script&gt;&lt;script&gt;alert(1)&lt;/script&gt;"));
    assert!(!html.contains("<script>"));
    assert!(html.contains("root input"));
    assert_eq!(html.matches("delivered &lt;answer&gt;").count(), 1);
    assert!(!html.contains("hidden reasoning"));
    assert!(preview("中".repeat(50_000)).ends_with("complete original remains in SQLite.]"));
}
#[test]
fn file_index_ignores_generated_hidden_and_symlinks() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("main.rs"), "").unwrap();
    fs::create_dir(dir.path().join("target")).unwrap();
    fs::write(dir.path().join("target/generated.rs"), "").unwrap();
    fs::write(dir.path().join(".hidden"), "").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(dir.path(), dir.path().join("loop")).unwrap();
    assert_eq!(files(dir.path()).unwrap(), vec!["main.rs"]);
}

#[test]
fn saved_drafts_are_backward_compatible_and_bounded_without_overwriting_good_state() {
    let dir = tempfile::tempdir().unwrap();
    let path = session_path(dir.path(), "session").unwrap();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, r#"{"draft":"legacy draft","history":[]}"#).unwrap();
    let mut saved = load(dir.path(), "session").unwrap();
    assert!(saved.drafts.is_empty());
    saved.drafts.insert(
        "".into(),
        SavedDraft {
            text: "original unsent request".into(),
            cursor: 8,
            selection: Some((0, 8)),
        },
    );
    save(dir.path(), "session", &saved).unwrap();
    assert_eq!(load(dir.path(), "session").unwrap(), saved);
    let before = fs::read(&path).unwrap();
    for i in 0..9 {
        saved.drafts.insert(
            format!("question-{i}"),
            SavedDraft {
                text: "x".repeat(DRAFT_LIMIT),
                ..Default::default()
            },
        );
    }
    assert!(save(dir.path(), "session", &saved).is_err());
    assert_eq!(fs::read(&path).unwrap(), before);
}
