use super::*;
use crate::tui::{App, Config, ConnectionChange, Engine, Profile, Settings, model_name};
use bone::state::Event;

fn fixture() -> (tempfile::TempDir, Engine, App) {
    let dir = tempfile::tempdir().unwrap();
    let profile = Profile::from_model("openai:initial-model").unwrap();
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
fn idle_model_switch_keeps_the_session_idle_without_fabricating_a_stop() {
    let (dir, mut engine, mut app) = fixture();
    let before = engine.events().unwrap();
    let revision = engine.state().revision;
    app.ui.paste("unsent requirement");
    app.set_model(&mut engine, &dir.path().join("data"), "new-native-id")
        .unwrap();
    assert!(!engine.state().paused);
    assert!(engine.is_quiescent());
    assert_eq!(engine.state().revision, revision);
    assert_eq!(engine.events().unwrap().len(), before.len());
    assert_eq!(app.ui.draft(), "unsent requirement");
    assert!(!app.ui.notice.contains("Ctrl+R"));
    let saved = Config::load(&dir.path().join("data")).unwrap();
    assert_eq!(
        model_name(saved.profile(Some("test")).unwrap()),
        "new-native-id"
    );
}

#[test]
fn changing_a_model_preserves_pending_work_and_its_input() {
    let (dir, mut engine, mut app) = fixture();
    let input = engine
        .post_message("unfinished engineering request")
        .unwrap();
    assert!(!engine.is_quiescent());
    app.set_model(&mut engine, &dir.path().join("data"), "next-model")
        .unwrap();
    assert!(!engine.state().paused);
    assert!(!engine.is_quiescent());
    assert_eq!(
        crate::tui::event_body(&engine, &engine.read_event(&input).unwrap()).unwrap(),
        "unfinished engineering request"
    );
    let events = engine.events().unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "stopped")
            .count(),
        0
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.kind.as_str(), "model_started" | "tool_started"))
    );
    assert!(!app.ui.notice.contains("Ctrl+R"));
}

#[test]
fn changing_a_model_does_not_stop_an_already_paused_session_again() {
    let (dir, mut engine, mut app) = fixture();
    engine.post_message("paused request").unwrap();
    engine.stop().unwrap();
    let count = engine.events().unwrap().len();
    let revision = engine.state().revision;
    app.set_model(&mut engine, &dir.path().join("data"), "next-model")
        .unwrap();
    assert!(engine.state().paused);
    assert_eq!(engine.state().revision, revision);
    assert_eq!(engine.events().unwrap().len(), count);
}

#[test]
fn idle_configuration_conflict_keeps_the_original_model_and_does_not_stop() {
    let (dir, mut engine, mut app) = fixture();
    let data = dir.path().join("data");
    let (config, revision) = Config::load_with_revision(&data).unwrap();
    let change = ConnectionChange {
        config,
        revision,
        name: "test".into(),
        profile: app
            .settings
            .profile
            .with_model("openai:rejected-model")
            .unwrap(),
    };
    let (mut external, revision) = Config::load_with_revision(&data).unwrap();
    external.profiles.insert(
        "external".into(),
        Profile::from_model("ollama:external-model").unwrap(),
    );
    external.default_profile = "external".into();
    external.save_checked(&data, &revision).unwrap();
    let bytes = std::fs::read(data.join("config.toml")).unwrap();
    assert!(app.apply_connection(&mut engine, &data, change).is_err());
    assert!(!engine.state().paused);
    assert_eq!(model_name(&app.settings.profile), "initial-model");
    assert!(engine.events().unwrap().is_empty());
    assert_eq!(std::fs::read(data.join("config.toml")).unwrap(), bytes);
}

#[test]
fn tool_classification_uses_observed_outcome_instead_of_copy_or_tool_name() {
    let (_dir, engine, mut app) = fixture();
    for (index, output, uncertain, expected) in [
        (
            0,
            r#"{"exit_code":0,"stdout":"失败 test passed; error.txt exists"}"#,
            false,
            ToolState::Completed,
        ),
        (
            1,
            r#"{"exit_code":2,"stderr":"done; all good"}"#,
            false,
            ToolState::Failed,
        ),
        (
            2,
            r#"{"exit_code":0,"stdout":"done"}"#,
            true,
            ToolState::Unknown,
        ),
    ] {
        let mut event = Event::new(
            &engine.state().id,
            "tool_result",
            serde_json::json!({
                "tool_name":"running_错误", "message":rig_core::completion::Message::user(output), "uncertain":uncertain
            }),
        );
        event.call_id = Some(format!("call-{index}"));
        app.ingest(&engine, &event).unwrap();
        let message = app
            .ui
            .messages
            .iter()
            .find(|message| message.event_id.as_deref() == Some(&format!("tool:call-{index}")))
            .unwrap();
        assert_eq!(
            message.kind,
            MessageKind::Tool {
                name: "running_错误".into(),
                state: expected
            }
        );
    }
}

#[test]
fn translated_or_contradictory_tool_copy_cannot_change_rendered_outcome() {
    use ratatui::{Terminal, backend::TestBackend, style::Color};
    let state = bone::state::SessionState::new("/tmp/work");
    for (outcome, color) in [
        (ToolState::Completed, Color::Green),
        (ToolState::Failed, Color::Red),
        (ToolState::Unknown, Color::Yellow),
        (ToolState::Running, Color::Blue),
    ] {
        let mut ui = View::new();
        ui.push_message(Message {
            kind: MessageKind::Tool {
                name: "evidence".into(),
                state: outcome,
            },
            text: "失败 · 完成 · 执行中 · 中文文案可以修改\n原始证据".into(),
            summary: Some("arbitrary translated summary".into()),
            event_id: Some("tool:evidence".into()),
        });
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        terminal
            .draw(|frame| ui.render(frame, &state, "model", "等待输入"))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let row = (0..40)
            .find(|&row| {
                let text = (0..120)
                    .map(|column| buffer[(column, row)].symbol())
                    .collect::<String>();
                text.contains("evidence")
            })
            .expect("the real transcript renders its tool label");
        assert_eq!(
            message_lines(&ui.messages[0], 80, false, false)[0].style.fg,
            Some(color)
        );
        if std::env::var_os("NO_COLOR").is_none() {
            assert!(
                (0..120).any(|column| buffer[(column, row)].fg == color),
                "{outcome:?}"
            );
        }
    }
}

#[test]
fn input_target_is_a_fact_independent_of_display_copy() {
    let (_dir, engine, mut app) = fixture();
    app.ui.reply_label = "anything, including 回复 or 新要求".into();
    app.metadata(&engine);
    assert_eq!(app.ui.input_target, InputTarget::Message);
    app.reply_target = Some("expired-but-still-explicit-question".into());
    app.metadata(&engine);
    assert_eq!(app.ui.input_target, InputTarget::Reply);
}

#[test]
fn live_observation_requires_an_active_tool_and_received_progress_not_a_chinese_prefix() {
    let (_dir, _engine, mut app) = fixture();
    app.ui.push_message(Message {
        kind: MessageKind::Tool {
            name: "shell".into(),
            state: ToolState::Running,
        },
        text: "a freely translated progress observation".into(),
        summary: None,
        event_id: Some("tool:running-call".into()),
    });
    app.ui.select_message("tool:running-call");
    assert!(
        app.selected_tool_observation().is_none(),
        "starting a tool is not received output"
    );
    app.tool_observations.insert(
        "running-call".into(),
        ("actual output".into(), String::new()),
    );
    assert!(app.selected_tool_observation().is_some());
    app.ui.upsert_message(Message {
        kind: MessageKind::Tool {
            name: "shell".into(),
            state: ToolState::Completed,
        },
        text: "执行中 · 实时日志 is quoted inside the final artifact".into(),
        summary: None,
        event_id: Some("tool:running-call".into()),
    });
    assert!(
        app.selected_tool_observation().is_none(),
        "quoted copy cannot turn a final result into live output"
    );
}
