use super::*;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

#[test]
fn modal_paste_targets_query_and_never_mutates_hidden_draft() {
    let mut view = View::new();
    view.paste("草稿");
    view.open_picker(PickerKind::History, vec![], "find".into());
    view.handle_paste("👩‍💻\n中");
    assert_eq!(
        view.picker.as_ref().map(|p| p.query.as_str()),
        Some("find👩‍💻 中")
    );
    assert_eq!(view.draft(), "草稿");
    view.handle_paste(&"x".repeat(4096));
    assert_eq!(
        view.picker.as_ref().map(|p| p.query.as_str()),
        Some("find👩‍💻 中")
    );
    view.handle_key(key(KeyCode::Esc));
    view.detail = Some(("详情".into(), "body".into()));
    view.handle_paste("hidden");
    assert_eq!(view.draft(), "草稿");
}

#[test]
fn oversized_paste_preserves_draft_cursor_and_undo() {
    let mut view = View::new();
    view.paste("原文👩‍💻");
    view.handle_key(key(KeyCode::Left));
    let before = (view.draft(), view.cursor());
    view.paste(&"中".repeat(50_000));
    assert_eq!((view.draft(), view.cursor()), before);
    assert!(view.notice.contains("128 KiB"));
    view.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
    assert!(view.draft().contains('\n'));
}
#[test]
fn stream_updates_retain_full_text_and_bounded_prompt_history() {
    let mut view = View::new();
    let text = "e\u{301}".repeat(20_000);
    view.upsert_message(Message {
        role: "AI".into(),
        text: "small".into(),
        event_id: Some("1".into()),
    });
    view.upsert_message(Message {
        role: "AI".into(),
        text: text.clone(),
        event_id: Some("1".into()),
    });
    assert_eq!(view.messages.len(), 1);
    assert_eq!(view.messages[0].text, text);
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
    assert_eq!(view.draft(), "任务");
}
#[test]
fn multiline_editor_history_and_undo_preserve_unicode() {
    let mut view = View::new();
    view.paste("中文\n👩‍💻a");
    view.handle_key(key(KeyCode::Up));
    assert!(view.draft().is_char_boundary(view.cursor()));
    view.handle_key(key(KeyCode::Down));
    assert!(view.draft().is_char_boundary(view.cursor()));
    view.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
    view.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));
    assert_eq!(view.draft(), "中文\n👩‍💻a");
    view.take_draft();
    view.remember_prompt("旧任务");
    view.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::ALT));
    assert_eq!(view.draft(), "旧任务");
    view.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::ALT));
    assert_eq!(view.draft(), "");
}
#[test]
fn markdown_styles_diff_and_bounds_cells() {
    let lines = markdown("# 标题\n`代码`\n```diff\n+新增\n-删除\n@@ chunk\n```", 8);
    assert!(lines.iter().any(|l| {
        l.spans
            .iter()
            .any(|s| s.style.add_modifier.contains(Modifier::BOLD))
            || l.style.add_modifier.contains(Modifier::BOLD)
    }));
    for line in lines {
        assert!(line.width() <= 8);
    }
}
#[test]
fn selected_message_positions_once_and_stream_updates_preserve_scroll() {
    let state = bone::state::SessionState::new("/tmp/work");
    use ratatui::{Terminal, backend::TestBackend};
    let mut view = View::new();
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    for i in 0..20 {
        view.push_message(Message {
            role: "AI".into(),
            text: "body".into(),
            event_id: Some(i.to_string()),
        });
    }
    view.select_message("5");
    terminal
        .draw(|f| view.render(f, &state, "m", "idle"))
        .unwrap();
    assert_eq!(view.conversation_scroll, view.message_offsets[5]);
    view.conversation_scroll += 3;
    let scroll = view.conversation_scroll;
    view.upsert_message(Message {
        role: "工具输出中".into(),
        text: "update".into(),
        event_id: Some("19".into()),
    });
    terminal
        .draw(|f| view.render(f, &state, "m", "idle"))
        .unwrap();
    assert_eq!(view.conversation_scroll, scroll);
    view.remove_message("19");
    assert!(view.cache_dirty);
}

#[test]
fn editing_preserves_graphemes_and_wide_characters() {
    let mut view = View::new();
    view.paste("中文👩‍💻e\u{301}");
    view.handle_key(key(KeyCode::Backspace));
    assert_eq!(view.draft(), "中文👩‍💻");
    view.handle_key(key(KeyCode::Left));
    view.handle_key(key(KeyCode::Delete));
    assert_eq!(view.draft(), "中文");
    view.handle_key(key(KeyCode::Left));
    view.paste("你好");
    assert_eq!(view.draft(), "中你好文");
}

#[test]
fn pasted_lines_and_enter_do_not_submit() {
    let mut view = View::new();
    view.paste("第一行\r\n第二行\r第三行");
    view.handle_key(key(KeyCode::Enter));
    assert_eq!(view.draft(), "第一行\n第二行\n第三行");
    assert_eq!(view.take_draft(), "第一行\n第二行\n第三行");
    assert!(view.draft().is_empty());
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
fn prepend_retains_reading_anchor_and_selected_message() {
    let mut view = View::new();
    view.cache_width = 8;
    view.push_message(Message {
        role: "Agent".into(),
        text: "current".into(),
        event_id: Some("current".into()),
    });
    view.select_message("current");
    view.selection_needs_scroll = false;
    view.conversation_scroll = 1;
    let old = Message {
        role: "Agent".into(),
        text: "older\nbody".into(),
        event_id: Some("old".into()),
    };
    let shift = message_lines(&old, 8, false, false).len();
    view.prepend_messages(vec![old]);
    assert_eq!(view.selected_message_id(), Some("current"));
    assert_eq!(view.conversation_scroll, 1 + shift);
    assert!(!view.follow_conversation);
    view.prepend_messages(vec![Message {
        role: "Agent".into(),
        text: "duplicate".into(),
        event_id: Some("old".into()),
    }]);
    assert_eq!(view.messages.len(), 2);
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
fn long_messages_expand_without_losing_original_text() {
    let mut view = View::new();
    let text = "中文原文\n".repeat(4000);
    view.push_message(Message {
        role: "AI".into(),
        text: text.clone(),
        event_id: Some("long".into()),
    });
    view.select_message("long");
    let folded = message_lines(view.selected_message().unwrap(), 80, false, true);
    assert!(folded.len() < 30);
    view.toggle_selected_message();
    let expanded = message_lines(view.selected_message().unwrap(), 80, true, true);
    assert!(expanded.len() > folded.len());
    assert_eq!(
        expanded
            .iter()
            .flat_map(|l| &l.spans)
            .map(|s| s.content.as_ref())
            .collect::<String>()
            .matches("中文原文")
            .count(),
        4000
    );
    assert_eq!(view.selected_message().unwrap().text, text);
}

#[test]
fn renders_narrow_wide_and_modal_views_without_terminal_controls() {
    let state = bone::state::SessionState::new("/tmp/中文\x07workspace");
    use ratatui::{Terminal, backend::TestBackend};
    for (width, height) in [(120, 40), (80, 24), (8, 6), (4, 3)] {
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

#[test]
fn inline_completion_preserves_typing_and_selection_paste_undo() {
    let mut view = View::new();
    view.paste("/mo");
    view.open_completion(
        PickerKind::Command,
        vec![PickerItem {
            label: "model".into(),
            detail: String::new(),
            value: "/model".into(),
        }],
        "mo".into(),
    );
    assert!(!view.has_modal());
    view.handle_key(key(KeyCode::Char('d')));
    assert_eq!(view.draft(), "/mod");
    view.handle_key(key(KeyCode::Esc));
    assert!(!view.is_completion());
    view.take_draft();
    view.paste("中文\nhello");
    view.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT));
    view.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT));
    assert_eq!(view.selected_input_text().as_deref(), Some("lo"));
    view.handle_paste("世界");
    assert_eq!(view.draft(), "中文\nhel世界");
    view.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));
    assert_eq!(view.draft(), "中文\nhel");
    view.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));
    assert_eq!(view.draft(), "中文\nhello");
    view.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::ALT));
    view.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::ALT));
    assert_eq!(view.draft(), "中文\nhel世界");
}
#[test]
fn conversation_navigation_and_tool_logs_are_readable() {
    let mut view = View::new();
    for index in 0..3 {
        view.push_message(Message {
            role: "工具 · shell 完成".into(),
            text: "# raw\n  indentation\n- literal".into(),
            event_id: Some(index.to_string()),
        });
    }
    view.focus = Focus::Conversation;
    view.handle_key(key(KeyCode::Up));
    assert_eq!(view.selected_message_id(), Some("1"));
    view.handle_key(key(KeyCode::Char('j')));
    assert_eq!(view.selected_message_id(), Some("2"));
    let lines = message_lines(view.selected_message().unwrap(), 80, true, true);
    assert_eq!(lines[1].spans[0].content, "# raw");
    assert_eq!(lines[2].spans[0].content, "  indentation");
}

#[test]
fn emoji_selection_and_completion_replacement_follow_grapheme_boundaries() {
    let mut view = View::new();
    view.paste("中👩‍💻e\u{301}");
    view.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT));
    assert_eq!(view.selected_input_text().as_deref(), Some("e\u{301}"));
    view.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT));
    assert_eq!(view.selected_input_text().as_deref(), Some("👩‍💻e\u{301}"));
    view.handle_paste("文");
    assert_eq!(view.draft(), "中文");
    view.replace_range(0.."中文".len(), "/model ");
    assert_eq!(view.draft(), "/model ");
    assert_eq!(view.cursor(), "/model ".len());
    view.replace_range(0..0, "");
    assert_eq!(view.draft(), "/model ");
}

#[test]
fn visual_line_navigation_keeps_wrapped_draft_and_inline_panel_visible() {
    use ratatui::{Terminal, backend::TestBackend};
    let state = bone::state::SessionState::new("/tmp/work");
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let mut view = View::new();
    view.remember_prompt("old prompt");
    let draft = "中文字".repeat(50);
    view.paste(&draft);
    terminal
        .draw(|f| view.render(f, &state, "m", "idle"))
        .unwrap();
    let cursor = view.cursor();
    view.handle_key(key(KeyCode::Up));
    assert!(view.cursor() < cursor);
    assert_eq!(view.draft(), draft);
    view.take_draft();
    view.paste("/mo");
    view.open_completion(
        PickerKind::Command,
        vec![PickerItem {
            label: "/model".into(),
            detail: "切换模型".into(),
            value: "/model".into(),
        }],
        "mo".into(),
    );
    terminal
        .draw(|f| view.render(f, &state, "m", "idle"))
        .unwrap();
    let text = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect::<String>();
    assert!(text.contains("/model"));
    assert!(text.contains("/mo"));
    assert!(text.contains("Tab"));
}

#[test]
fn bounded_window_evicts_opposite_edge_and_retains_draft() {
    let mut view = View::new();
    view.paste("independent draft");
    for i in 0..256 {
        view.push_message(Message {
            role: "AI".into(),
            text: "body".into(),
            event_id: Some(i.to_string()),
        });
    }
    view.select_message("0");
    view.push_message(Message {
        role: "AI".into(),
        text: "new".into(),
        event_id: Some("256".into()),
    });
    assert_eq!(view.messages.len(), 256);
    assert_eq!(view.first_message_id(), Some("1"));
    assert!(view.selected_message().is_none());
    assert_eq!(view.draft(), "independent draft");
    view.select_message("1");
    view.prepend_messages(vec![Message {
        role: "AI".into(),
        text: "older".into(),
        event_id: Some("old".into()),
    }]);
    assert_eq!(view.messages.len(), 256);
    assert_eq!(view.first_message_id(), Some("old"));
    assert_eq!(view.selected_message_id(), Some("1"));
    assert_eq!(
        view.messages.last().unwrap().event_id.as_deref(),
        Some("255")
    );
    for i in 0..80 {
        view.upsert_message(Message {
            role: "工具完成".into(),
            text: "👩‍💻".repeat(30_000),
            event_id: Some(format!("big{i}")),
        });
    }
    assert!(
        view.messages
            .iter()
            .map(|m| m.text.len() + m.role.len() + m.event_id.as_ref().map_or(0, String::len))
            .sum::<usize>()
            <= 8 * 1024 * 1024
    );
    assert!(view.messages.iter().all(|m| m.text.len() <= 128 * 1024));
    assert!(
        view.messages
            .last()
            .unwrap()
            .text
            .ends_with("[显示预览，d 原文 / y 复制完整记录]")
    );
}

#[test]
fn persisted_history_results_are_not_filtered_by_snippet_and_modal_hides_selection() {
    let mut view = View::new();
    view.paste("draft");
    view.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT));
    assert_eq!(view.selected_input_text().as_deref(), Some("t"));
    view.open_picker(
        PickerKind::History,
        vec![PickerItem {
            label: "message".into(),
            detail: "short snippet".into(),
            value: "event".into(),
        }],
        "long query absent from truncated snippet".into(),
    );
    assert_eq!(
        view.picker_value(),
        Some((PickerKind::History, "event".into()))
    );
    assert_eq!(view.selected_input_text(), None);
    view.handle_key(key(KeyCode::Esc));
    assert_eq!(view.selected_input_text().as_deref(), Some("t"));
    view.detail = Some(("raw".into(), "body".into()));
    assert_eq!(view.selected_input_text(), None);
}

#[test]
fn narrow_conversation_focus_selects_latest_and_tool_status_is_explicit() {
    use ratatui::{Terminal, backend::TestBackend};
    let state = bone::state::SessionState::new("/tmp/work");
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let mut view = View::new();
    view.push_message(Message {
        role: "AI".into(),
        text: "latest".into(),
        event_id: Some("latest".into()),
    });
    terminal
        .draw(|f| view.render(f, &state, "m", "idle"))
        .unwrap();
    view.handle_key(key(KeyCode::Tab));
    assert_eq!(view.focus, Focus::Conversation);
    assert_eq!(view.selected_message_id(), Some("latest"));
    for (text, color) in [
        ("执行中 · command", Color::Blue),
        ("结果未知 · 需核查", Color::Yellow),
        ("失败 · command", Color::Red),
        ("完成 · command", Color::Green),
        ("完成 · 未知错误文件.txt", Color::Green),
    ] {
        let message = Message {
            role: "工具 · shell".into(),
            text: text.into(),
            event_id: None,
        };
        assert_eq!(
            message_lines(&message, 78, false, false)[0].style.fg,
            Some(color)
        );
    }
}
