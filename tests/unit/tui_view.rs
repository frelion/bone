use super::*;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

#[test]
fn modal_paste_targets_query_and_never_mutates_hidden_draft() {
    let mut view = View::new();
    view.paste("草稿");
    view.start_search("find");
    view.handle_paste("👩‍💻\n中");
    assert_eq!(view.search.as_deref(), Some("find👩‍💻 中"));
    assert_eq!(view.draft, "草稿");
    view.handle_paste(&"x".repeat(4096));
    assert_eq!(view.search.as_deref(), Some("find👩‍💻 中"));
    view.handle_key(key(KeyCode::Esc));
    view.detail = Some(("详情".into(), "body".into()));
    view.handle_paste("hidden");
    assert_eq!(view.draft, "草稿");
}

#[test]
fn oversized_paste_preserves_draft_cursor_and_undo() {
    let mut view = View::new();
    view.paste("原文👩‍💻");
    view.handle_key(key(KeyCode::Left));
    let before = (view.draft.clone(), view.cursor, view.undo.len());
    view.paste(&"中".repeat(50_000));
    assert_eq!((view.draft.clone(), view.cursor, view.undo.len()), before);
    assert!(view.notice.contains("128 KiB"));
    view.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
    assert!(view.draft.contains('\n'));
}
#[test]
fn stream_updates_cannot_exceed_history_preview_bounds() {
    let mut view = View::new();
    for i in 0..20 {
        view.push_message(Message {
            role: "AI".into(),
            text: "small".into(),
            event_id: Some(i.to_string()),
        });
    }
    for i in 0..20 {
        view.upsert_message(Message {
            role: "AI".into(),
            text: "e\u{301}".repeat(20_000),
            event_id: Some(i.to_string()),
        });
    }
    assert!(
        view.messages
            .iter()
            .map(|m| m.text.chars().count() + m.role.chars().count())
            .sum::<usize>()
            <= 160_000
    );
    for message in &view.messages {
        assert!(message.text.chars().count() <= 16_000);
        assert!(!message.text.ends_with('e'));
    }
    for i in 0..20 {
        view.remember_prompt(&format!("{i}{}", "中".repeat(30_000)));
    }
    assert!(view.history.iter().map(String::len).sum::<usize>() <= 1024 * 1024);
}

#[test]
fn picker_filters_selects_and_closes_without_editing_prompt() {
    let mut view = View::new();
    view.paste("任务");
    view.open_picker(
        PickerKind::Command,
        vec![
            PickerItem {
                label: "Sessions".into(),
                detail: "Resume history".into(),
                value: "/resume".into(),
            },
            PickerItem {
                label: "Model".into(),
                detail: String::new(),
                value: "/model".into(),
            },
        ],
        "ssn".into(),
    );
    assert_eq!(
        view.picker_value(),
        Some((PickerKind::Command, "/resume".into()))
    );
    view.handle_key(key(KeyCode::Esc));
    assert!(view.picker.is_none());
    assert_eq!(view.draft, "任务");
}
#[test]
fn multiline_editor_history_and_undo_preserve_unicode() {
    let mut view = View::new();
    view.paste("中文\n👩‍💻a");
    view.handle_key(key(KeyCode::Up));
    assert_eq!(view.cursor, "中".len());
    view.handle_key(key(KeyCode::Down));
    assert_eq!(view.cursor, "中文\n👩‍💻".len());
    view.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
    view.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));
    assert_eq!(view.draft, "中文\n👩‍💻a");
    view.take_draft();
    view.remember_prompt("旧任务");
    view.handle_key(key(KeyCode::Up));
    assert_eq!(view.draft, "旧任务");
    view.handle_key(key(KeyCode::Down));
    assert_eq!(view.draft, "");
}
#[test]
fn markdown_styles_diff_and_bounds_cells() {
    let lines = markdown("# 标题\n`代码`\n```diff\n+新增\n-删除\n@@ chunk\n```", 8);
    assert!(lines.iter().any(|l| l.style.fg == Some(Color::Green)
        || l.spans.iter().any(|s| s.style.fg == Some(Color::Green))));
    assert!(lines.iter().any(|l| l.style.fg == Some(Color::Red)
        || l.spans.iter().any(|s| s.style.fg == Some(Color::Red))));
    for line in lines {
        assert!(line.width() <= 8);
    }
}
#[test]
fn transcript_cache_invalidates_and_search_is_explicit() {
    let state = bone::state::SessionState::new("/tmp/work");
    use ratatui::{Terminal, backend::TestBackend};
    let mut view = View::new();
    let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
    view.upsert_message(Message {
        role: "AI".into(),
        text: "needle".into(),
        event_id: Some("1".into()),
    });
    view.start_search("needle");
    terminal
        .draw(|f| view.render(f, &state, "m", "idle"))
        .unwrap();
    assert!(!view.cache_dirty);
    assert!(view.search.is_some());
    view.handle_key(key(KeyCode::Esc));
    assert!(view.search.is_none());
    view.remove_message("1");
    assert!(view.messages.is_empty());
    assert!(view.cache_dirty);
}

#[test]
fn editing_preserves_graphemes_and_wide_characters() {
    let mut view = View::new();
    view.paste("中文👩‍💻e\u{301}");
    view.handle_key(key(KeyCode::Backspace));
    assert_eq!(view.draft, "中文👩‍💻");
    view.handle_key(key(KeyCode::Left));
    view.handle_key(key(KeyCode::Delete));
    assert_eq!(view.draft, "中文");
    view.handle_key(key(KeyCode::Left));
    view.paste("你好");
    assert_eq!(view.draft, "中你好文");
}

#[test]
fn pasted_lines_and_enter_do_not_submit() {
    let mut view = View::new();
    view.paste("第一行\r\n第二行\r第三行");
    view.handle_key(key(KeyCode::Enter));
    assert_eq!(view.draft, "第一行\n第二行\n第三行");
    assert_eq!(view.take_draft(), "第一行\n第二行\n第三行");
    assert!(view.draft.is_empty());
}

#[test]
fn strips_terminal_sequences_and_controls() {
    let safe = View::sanitize("中文\x1b[31m红\x1b[0m\x07\r\x1b]52;c;secret\x07\n\t好\u{85}");
    assert_eq!(safe, "中文红\n    好");
    assert!(safe.chars().all(|c| !c.is_control() || c == '\n'));
}

#[test]
fn cursor_uses_cells_after_wrapping() {
    let (lines, row, col) = wrap_with_cursor("中文a", "中文a".len(), 4);
    assert_eq!(lines, ["中文", "a"]);
    assert_eq!((row, col), (1, 1));
    let (_, row, col) = wrap_with_cursor("e\u{301}👩‍💻", "e\u{301}👩‍💻".len(), 8);
    assert_eq!((row, col), (0, 3));
}

#[test]
fn scrolled_markdown_eviction_keeps_the_visible_message() {
    let mut view = View::new();
    view.cache_width = 8;
    view.push_message(Message {
        role: "Agent".into(),
        text: "```text\nabcdefgh\n```".into(),
        event_id: None,
    });
    for index in 1..150 {
        view.push_message(Message {
            role: "Agent".into(),
            text: format!("item {index}"),
            event_id: None,
        });
    }
    // Two extra lines frame each message. Code indentation wraps its body
    // onto two lines, so the first message occupies six rendered lines.
    view.follow_conversation = false;
    view.conversation_scroll = 9;
    view.conversation_max = 450;
    view.push_message(Message {
        role: "Agent".into(),
        text: "item 150".into(),
        event_id: None,
    });
    assert_eq!(view.messages[0].text, "item 1");
    assert_eq!(view.conversation_scroll, 3);
    assert_eq!(view.conversation_max, 444);
    assert!(!view.follow_conversation);
}

#[test]
fn activity_eviction_retains_selected_event() {
    let mut view = View::new();
    for index in 0..400 {
        view.push_activity(Activity {
            event_id: index.to_string(),
            title: "行动".into(),
            detail: String::new(),
            tone: Tone::Normal,
        });
    }
    view.selected = 100;
    view.activity_top = 90;
    view.push_activity(Activity {
        event_id: "400".into(),
        title: "行动".into(),
        detail: String::new(),
        tone: Tone::Normal,
    });
    assert_eq!(view.activities.len(), 400);
    assert_eq!(view.selected_event(), Some("100"));
    assert_eq!(view.activity_top, 89);
}

#[test]
fn message_preview_limits_are_unicode_safe_and_bounded() {
    let mut view = View::new();
    for index in 0..160 {
        view.push_message(Message {
            role: "用户".into(),
            text: "中文".into(),
            event_id: Some(index.to_string()),
        });
    }
    assert_eq!(view.messages.len(), 150);
    assert_eq!(
        view.messages.first().unwrap().event_id.as_deref(),
        Some("10")
    );
    for _ in 0..12 {
        view.push_message(Message {
            role: "AI".into(),
            text: "\x1b[31m中".repeat(20_000),
            event_id: None,
        });
    }
    let text = &view.messages.last().unwrap().text;
    assert_eq!(text.chars().count(), 16_000);
    assert!(text.ends_with("[界面预览已省略，原文保留在会话记录]"));
    assert!(
        view.messages
            .iter()
            .map(|m| m.text.chars().count() + m.role.chars().count())
            .sum::<usize>()
            <= 160_000
    );
}

#[test]
fn renders_narrow_wide_and_modal_views_without_terminal_controls() {
    let state = bone::state::SessionState::new("/tmp/中文\x07workspace");
    use ratatui::{Terminal, backend::TestBackend};
    for (width, height) in [(120, 30), (80, 20), (8, 6), (4, 3)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let mut view = View::new();
        view.paste("中文\n👩‍💻\n第三行");
        view.push_message(Message {
            role: "用户".into(),
            text: "\x1b[31m任务\x07详情".into(),
            event_id: None,
        });
        view.push_activity(Activity {
            event_id: "event-1".into(),
            title: "工具完成".into(),
            detail: "原生事件".into(),
            tone: Tone::Success,
        });
        view.notice = "状态\r\x07正常".into();
        terminal
            .draw(|frame| view.render(frame, &state, "model\x1b[0m", "idle\x07"))
            .unwrap();
        for cell in terminal.backend().buffer().content() {
            assert!(cell.symbol().chars().all(|c| !c.is_control()));
        }
        if width == 80 {
            view.handle_key(key(KeyCode::F(2)));
            view.handle_key(key(KeyCode::Tab));
            terminal
                .draw(|frame| view.render(frame, &state, "model", "idle"))
                .unwrap();
            assert_eq!(view.focus, Focus::Activity);
            assert_eq!(view.selected_event(), Some("event-1"));
            let text = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            assert!(text.replace(' ', "").contains("原生事件"));
            view.handle_key(key(KeyCode::Tab));
            assert_eq!(view.focus, Focus::Input);
            view.handle_key(key(KeyCode::F(2)));
            view.handle_key(key(KeyCode::Tab));
            assert_eq!(view.focus, Focus::Conversation);
        }
        view.show_help = true;
        terminal
            .draw(|frame| view.render(frame, &state, "model", "idle"))
            .unwrap();
        view.show_help = false;
        view.detail = Some(("事件".into(), "详情\n".repeat(40)));
        terminal
            .draw(|frame| view.render(frame, &state, "model", "idle"))
            .unwrap();
    }
}
