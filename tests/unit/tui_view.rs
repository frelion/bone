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
    view.detail = None;
    view.toggle_inspector();
    let cursor = view.cursor();
    view.handle_paste("audit paste");
    assert_eq!(view.draft(), "草稿");
    assert_eq!(view.cursor(), cursor);
    assert_eq!(view.focus, Focus::Activity);
    assert!(view.show_activity);
    assert!(view.notice.contains("F6"));
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
        kind: MessageKind::Delivery,
        text: "small".into(),
        event_id: Some("1".into()),
        summary: None,
    });
    view.upsert_message(Message {
        kind: MessageKind::Delivery,
        text: text.clone(),
        event_id: Some("1".into()),
        summary: None,
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
            kind: MessageKind::Delivery,
            text: "body".into(),
            event_id: Some(i.to_string()),
            summary: None,
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
        kind: MessageKind::Tool {
            name: "shell".into(),
            state: ToolState::Running,
        },
        text: "update".into(),
        event_id: Some("19".into()),
        summary: None,
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
        kind: MessageKind::Delivery,
        text: "current".into(),
        event_id: Some("current".into()),
        summary: None,
    });
    view.select_message("current");
    view.selection_needs_scroll = false;
    view.conversation_scroll = 1;
    let old = Message {
        kind: MessageKind::Delivery,
        text: "older\nbody".into(),
        event_id: Some("old".into()),
        summary: None,
    };
    let shift = message_lines(&old, 8, false, false).len();
    view.prepend_messages(vec![old]);
    assert_eq!(view.selected_message_id(), Some("current"));
    assert_eq!(view.conversation_scroll, 1 + shift);
    assert!(!view.follow_conversation);
    view.prepend_messages(vec![Message {
        kind: MessageKind::Delivery,
        text: "duplicate".into(),
        event_id: Some("old".into()),
        summary: None,
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
fn long_agent_delivery_remains_readable_without_expanding() {
    let mut view = View::new();
    let text = format!("{}\n交付尾部", "中文原文\n".repeat(4000));
    view.push_message(Message {
        kind: MessageKind::Delivery,
        text: text.clone(),
        event_id: Some("long".into()),
        summary: None,
    });
    view.select_message("long");
    let folded = message_lines(view.selected_message().unwrap(), 80, false, true);
    assert!(folded.len() > 30);
    view.toggle_selected_message();
    let expanded = message_lines(view.selected_message().unwrap(), 80, true, true);
    assert_eq!(expanded.len(), folded.len());
    let visible_text = |rows: &[Line<'_>]| {
        rows.iter()
            .flat_map(|line| {
                line.spans
                    .iter()
                    .enumerate()
                    .filter(|(index, span)| {
                        !(*index == 0 && matches!(span.content.as_ref(), "› " | "  "))
                    })
                    .flat_map(|(_, span)| {
                        span.content
                            .chars()
                            .filter(|character| !character.is_whitespace())
                    })
            })
            .collect::<String>()
    };
    let expected = text
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    for rows in [&folded, &expanded] {
        let visible = visible_text(rows);
        assert_eq!(visible, expected);
        assert_eq!(visible.matches("中文原文").count(), 4000);
        assert!(visible.ends_with("交付尾部"));
    }
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
            kind: MessageKind::User { admitted: false },
            text: "\x1b[31m任务\x07详情".into(),
            event_id: None,
            summary: None,
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
            view.handle_key(key(KeyCode::Esc));
            assert_eq!(view.focus, Focus::Input);
            view.handle_key(key(KeyCode::F(6)));
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
    view.handle_key(KeyEvent::new(
        KeyCode::Left,
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ));
    view.handle_key(KeyEvent::new(
        KeyCode::Left,
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ));
    assert_eq!(view.selected_input_text().as_deref(), Some("lo"));
    use ratatui::{Terminal, backend::TestBackend};
    let state = bone::state::SessionState::new("/tmp/work");
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal
        .draw(|frame| {
            view.render(frame, &state, "m", "idle");
            // Monochrome terminals discard color while preserving text attributes.
            for cell in &mut frame.buffer_mut().content {
                cell.fg = Color::Reset;
                cell.bg = Color::Reset;
            }
        })
        .unwrap();
    let position = terminal.backend().cursor_position();
    let cursor = &terminal.backend().buffer()[(position.x, position.y)];
    let selected_tail = &terminal.backend().buffer()[(position.x + 1, position.y)];
    assert!(terminal.backend().cursor_visible());
    assert_eq!(cursor.symbol(), "l");
    assert!(cursor.modifier.contains(Modifier::REVERSED));
    assert_eq!(selected_tail.symbol(), "o");
    assert!(selected_tail.modifier.contains(Modifier::UNDERLINED));
    assert!(!selected_tail.modifier.contains(Modifier::REVERSED));
    assert!(
        !terminal.backend().buffer()[(position.x - 1, position.y)]
            .modifier
            .contains(Modifier::UNDERLINED)
    );
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
            kind: MessageKind::Tool {
                name: "shell".into(),
                state: ToolState::Completed,
            },
            text: "# raw\n  indentation\n- literal".into(),
            event_id: Some(index.to_string()),
            summary: None,
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
    view.handle_key(KeyEvent::new(
        KeyCode::Left,
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ));
    assert_eq!(view.selected_input_text().as_deref(), Some("e\u{301}"));
    view.handle_key(KeyEvent::new(
        KeyCode::Left,
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ));
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
            kind: MessageKind::Delivery,
            text: "body".into(),
            event_id: Some(i.to_string()),
            summary: None,
        });
    }
    view.select_message("0");
    view.push_message(Message {
        kind: MessageKind::Delivery,
        text: "new".into(),
        event_id: Some("256".into()),
        summary: None,
    });
    assert_eq!(view.messages.len(), 256);
    assert_eq!(view.first_message_id(), Some("1"));
    assert!(view.selected_message().is_none());
    assert_eq!(view.draft(), "independent draft");
    view.select_message("1");
    view.prepend_messages(vec![Message {
        kind: MessageKind::Delivery,
        text: "older".into(),
        event_id: Some("old".into()),
        summary: None,
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
            kind: MessageKind::Tool {
                name: "shell".into(),
                state: ToolState::Completed,
            },
            text: "👩‍💻".repeat(30_000),
            event_id: Some(format!("big{i}")),
            summary: None,
        });
    }
    assert!(
        view.messages
            .iter()
            .map(|m| m.text.len()
                + m.event_id.as_ref().map_or(0, String::len)
                + match &m.kind {
                    MessageKind::Tool { name, .. } => name.len(),
                    _ => 0,
                })
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
    view.handle_key(KeyEvent::new(
        KeyCode::Left,
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ));
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
        kind: MessageKind::Delivery,
        text: "latest".into(),
        event_id: Some("latest".into()),
        summary: None,
    });
    terminal
        .draw(|f| view.render(f, &state, "m", "idle"))
        .unwrap();
    view.handle_key(key(KeyCode::F(6)));
    assert_eq!(view.focus, Focus::Conversation);
    assert_eq!(view.selected_message_id(), Some("latest"));
    for (text, state, color) in [
        ("执行中 · command", ToolState::Running, Color::Blue),
        ("结果未知 · 需核查", ToolState::Unknown, Color::Yellow),
        ("失败 · command", ToolState::Failed, Color::Red),
        ("完成 · command", ToolState::Completed, Color::Green),
        (
            "完成 · 未知错误文件.txt",
            ToolState::Completed,
            Color::Green,
        ),
    ] {
        let message = Message {
            kind: MessageKind::Tool {
                name: "shell".into(),
                state,
            },
            text: text.into(),
            event_id: None,
            summary: None,
        };
        assert_eq!(
            message_lines(&message, 78, false, false)[0].style.fg,
            Some(color)
        );
    }
}

#[test]
fn reading_layers_return_to_parent_and_preserve_draft_selection_and_undo() {
    let mut view = View::new();
    view.paste("original👩‍💻");
    view.handle_key(KeyEvent::new(
        KeyCode::Left,
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ));
    let cursor = view.cursor();
    let selection = view.selected_input_text();
    view.open_detail("结果", "result");
    view.detail_scroll = 7;
    view.open_picker(PickerKind::Command, vec![], String::new());
    view.open_help();
    assert!(view.show_help);
    assert!(view.close_layer());
    assert!(view.picker.is_some());
    assert!(view.close_layer());
    assert_eq!(
        view.detail.as_ref().map(|detail| detail.0.as_str()),
        Some("结果")
    );
    assert_eq!(view.detail_scroll, 7);
    assert!(view.close_layer());
    assert_eq!(view.focus, Focus::Input);
    assert_eq!(view.cursor(), cursor);
    assert_eq!(view.selected_input_text(), selection);
    view.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));
    assert_eq!(view.draft(), "");
}

#[test]
fn temporary_reconciliation_draft_restores_reader_and_original_editor() {
    let mut view = View::new();
    view.paste("草稿👩‍💻");
    view.reply_label = "回答 Q1".into();
    view.handle_key(KeyEvent::new(
        KeyCode::Left,
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ));
    let cursor = view.cursor();
    view.open_detail("旧结果", "captured result");
    view.detail_scroll = 13;
    view.begin_temporary_draft();
    assert!(!view.has_modal());
    assert_eq!(view.focus, Focus::Input);
    assert_eq!(view.temporary_draft_text().as_deref(), Some("草稿👩‍💻"));
    view.paste("核查备注");
    view.open_detail("核查证据", "evidence");
    view.close_layer();
    assert_eq!(view.draft(), "核查备注");
    assert!(view.restore_temporary_draft());
    assert_eq!(
        view.detail.as_ref().map(|detail| detail.0.as_str()),
        Some("旧结果")
    );
    assert_eq!(view.detail_scroll, 13);
    assert_eq!(view.draft(), "草稿👩‍💻");
    assert_eq!(view.reply_label, "回答 Q1");
    view.close_layer();
    assert_eq!(view.cursor(), cursor);
    assert_eq!(view.selected_input_text().as_deref(), Some("👩‍💻"));
}

#[test]
fn tab_keeps_focus_and_f6_switches_reading_and_input() {
    let mut view = View::new();
    view.push_message(Message {
        kind: MessageKind::Delivery,
        text: "delivery".into(),
        event_id: Some("e".into()),
        summary: None,
    });
    view.handle_key(key(KeyCode::Tab));
    assert_eq!(view.focus, Focus::Input);
    view.handle_key(key(KeyCode::F(6)));
    assert_eq!(view.focus, Focus::Conversation);
    view.handle_key(key(KeyCode::Tab));
    assert_eq!(view.focus, Focus::Conversation);
    view.handle_key(key(KeyCode::F(6)));
    assert_eq!(view.focus, Focus::Input);
}

#[test]
fn successful_tools_are_one_summary_and_failure_shows_reason_first() {
    let success = Message {
        kind: MessageKind::Tool {
            name: "shell".into(),
            state: ToolState::Completed,
        },
        text: "完成\nfull output\nextra output".into(),
        event_id: Some("s".into()),
        summary: Some("cargo check · exit 0".into()),
    };
    let compact = message_lines(&success, 80, false, false);
    assert_eq!(compact.len(), 1);
    let content = compact[0]
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect::<String>();
    assert!(content.contains("cargo check · exit 0"));
    assert!(!content.contains("full output"));
    assert!(message_lines(&success, 80, true, false).len() > compact.len());
    let failure = Message {
        kind: MessageKind::Tool {
            name: "shell".into(),
            state: ToolState::Failed,
        },
        text: "失败\nerror[E0425]: missing value\nstdout\nstderr".into(),
        event_id: Some("f".into()),
        summary: None,
    };
    let compact = message_lines(&failure, 50, false, false);
    assert!(
        compact[0].spans[0]
            .content
            .starts_with("  失败：error[E0425]")
    );
    assert_eq!(compact[0].style.fg, Some(Color::Red));
}

#[test]
fn new_output_preserves_source_location_on_resize_and_end_reaches_latest() {
    use ratatui::{Terminal, backend::TestBackend};
    let state = bone::state::SessionState::new("/tmp/work");
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let mut view = View::new();
    let text = (0..100)
        .map(|n| format!("paragraph {n:03}: {}\n", "source text ".repeat(12)))
        .collect::<String>();
    view.push_message(Message {
        kind: MessageKind::Delivery,
        text: text.clone(),
        event_id: Some("old".into()),
        summary: None,
    });
    terminal
        .draw(|f| view.render(f, &state, "hidden-model", "idle"))
        .unwrap();
    view.focus = Focus::Conversation;
    view.follow_conversation = false;
    view.conversation_scroll = 60;
    let before = view.read_points[60].offset;
    view.push_message(Message {
        kind: MessageKind::Delivery,
        text: "new delivery".into(),
        event_id: Some("new".into()),
        summary: None,
    });
    view.upsert_message(Message {
        kind: MessageKind::Delivery,
        text: format!("{text}extra tail"),
        event_id: Some("old".into()),
        summary: None,
    });
    let mut narrow = Terminal::new(TestBackend::new(42, 24)).unwrap();
    narrow
        .draw(|f| view.render(f, &state, "hidden-model", "idle"))
        .unwrap();
    let after = &view.read_points[view.conversation_scroll];
    assert_eq!(after.key, "old");
    assert!(after.offset <= before);
    assert!(before - after.offset < 42);
    assert!(view.unread > 0);
    let screen = narrow
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(screen.replace(' ', "").contains("新内容更新"));
    assert!(screen.contains("hidden-model"));
    view.handle_key(key(KeyCode::End));
    assert!(view.follow_conversation);
    assert_eq!(view.unread, 0);
    narrow
        .draw(|f| view.render(f, &state, "hidden-model", "idle"))
        .unwrap();
    assert_eq!(view.conversation_scroll, view.conversation_max);
}

#[test]
fn replacement_explains_when_live_observation_is_missing_from_final_result() {
    use ratatui::{Terminal, backend::TestBackend};
    let state = bone::state::SessionState::new("/tmp/work");
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let mut view = View::new();
    view.push_message(Message {
        kind: MessageKind::Tool {
            name: "shell".into(),
            state: ToolState::Running,
        },
        text: "observed data\n".repeat(40),
        event_id: Some("tool:call".into()),
        summary: None,
    });
    view.select_message("tool:call");
    view.toggle_selected_message();
    terminal
        .draw(|f| view.render(f, &state, "m", "running"))
        .unwrap();
    view.conversation_scroll = 8;
    view.upsert_message(Message {
        kind: MessageKind::Tool {
            name: "shell".into(),
            state: ToolState::Completed,
        },
        text: "完成\nfinal different output".into(),
        event_id: Some("tool:call".into()),
        summary: Some("exit 0".into()),
    });
    terminal
        .draw(|f| view.render(f, &state, "m", "idle"))
        .unwrap();
    assert!(view.notice.contains("原观察未包含"));
    assert_eq!(view.selected_message_id(), Some("tool:call"));
    assert!(!view.follow_conversation);
}

#[test]
fn help_scrolls_and_returns_to_detail_at_same_offset() {
    use ratatui::{Terminal, backend::TestBackend};
    let state = bone::state::SessionState::new("/tmp/work");
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let mut view = View::new();
    view.open_detail("结果", "line\n".repeat(100));
    terminal
        .draw(|f| view.render(f, &state, "m", "idle"))
        .unwrap();
    view.detail_scroll = 23;
    view.open_help();
    terminal
        .draw(|f| view.render(f, &state, "m", "idle"))
        .unwrap();
    view.handle_key(key(KeyCode::PageDown));
    assert!(view.detail_scroll > 0);
    view.handle_key(key(KeyCode::Esc));
    assert_eq!(view.detail_scroll, 23);
    assert_eq!(
        view.detail.as_ref().map(|detail| detail.0.as_str()),
        Some("结果")
    );
}

#[test]
fn running_tool_keeps_both_live_streams_observable_without_full_logs() {
    let message = Message { kind: MessageKind::Tool { name: "shell".into(), state: ToolState::Running }, text: "执行中 · preview\nstdout:\nfirst\ncompiling module\nstderr:\nold warning\nwarning: latest\n".into(), event_id: Some("tool:c".into()), summary: None };
    let rows = message_lines(&message, 80, false, false);
    let content = rows
        .iter()
        .flat_map(|line| &line.spans)
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(rows.len() <= 3);
    assert!(content.contains("stdout: compiling module"));
    assert!(content.contains("stderr: warning: latest"));
    assert!(!content.contains("old warning"));
}

#[test]
fn audit_detail_resize_retains_raw_line_and_esc_returns_original_reading() {
    use ratatui::{Terminal, backend::TestBackend};
    let state = bone::state::SessionState::new("/tmp/work");
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let mut view = View::new();
    view.focus = Focus::Conversation;
    view.conversation_scroll = 17;
    view.follow_conversation = false;
    view.toggle_inspector();
    let text = (0..300)
        .map(|line| format!("line{line:03}_{}\n", "abcdef0123456789".repeat(6)))
        .collect::<String>();
    view.open_detail("captured raw", text);
    terminal
        .draw(|f| view.render(f, &state, "m", "idle"))
        .unwrap();
    let text = &view.detail.as_ref().unwrap().1;
    let rows = wrap(text, view.detail_width)
        .into_iter()
        .map(Line::from)
        .collect::<Vec<_>>();
    view.detail_scroll = source_offsets(text, &rows)
        .iter()
        .position(|offset| text[*offset..].starts_with("line132_"))
        .unwrap();
    terminal
        .draw(|f| view.render(f, &state, "m", "idle"))
        .unwrap();
    let mut narrow = Terminal::new(TestBackend::new(42, 24)).unwrap();
    narrow
        .draw(|f| view.render(f, &state, "m", "idle"))
        .unwrap();
    let screen = narrow
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(screen.contains("line132_"));
    view.close_layer();
    assert!(view.show_activity);
    assert_eq!(view.focus, Focus::Activity);
    view.close_layer();
    assert_eq!(view.focus, Focus::Conversation);
    assert_eq!(view.conversation_scroll, 17);
    assert!(!view.follow_conversation);
}

#[test]
fn async_audit_arrival_returns_to_original_event_after_detail() {
    let mut view = View::new();
    view.push_activity(Activity {
        event_id: "original".into(),
        title: "old result".into(),
        detail: String::new(),
        tone: Tone::Normal,
    });
    view.toggle_inspector();
    view.open_detail("old result", "captured raw");
    view.push_activity(Activity {
        event_id: "new".into(),
        title: "new result".into(),
        detail: String::new(),
        tone: Tone::Normal,
    });
    view.close_layer();
    assert!(view.show_activity);
    assert_eq!(view.selected_event(), Some("original"));
    assert_eq!(view.activity_top, 0);
}

#[test]
fn markdown_table_resize_stays_on_row_before_later_literal_border() {
    use ratatui::{Terminal, backend::TestBackend};
    let state = bone::state::SessionState::new("/tmp/work");
    let payload = "payload_0123456789abcdefghijklmnopqrstuv";
    let mut text = format!("| row000 | {payload} |\n|---|---|\n");
    for row in 1..100 {
        text.push_str(&format!("| row{row:03} | {payload} |\n"));
    }
    text.push_str("\n─ trailer\n");
    let mut view = View::new();
    view.push_message(Message {
        kind: MessageKind::Delivery,
        text,
        event_id: Some("table".into()),
        summary: None,
    });
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal
        .draw(|f| view.render(f, &state, "m", "idle"))
        .unwrap();
    view.conversation_scroll = view
        .transcript_cache
        .iter()
        .position(|line| {
            line.spans
                .iter()
                .any(|span| span.content.contains("row012"))
        })
        .expect("table row is rendered");
    view.follow_conversation = false;
    view.focus = Focus::Conversation;
    let mut narrow = Terminal::new(TestBackend::new(42, 24)).unwrap();
    narrow
        .draw(|f| view.render(f, &state, "m", "idle"))
        .unwrap();
    let top = narrow
        .backend()
        .buffer()
        .content()
        .chunks(42)
        .nth(2)
        .unwrap()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(
        top.contains("row012"),
        "resize must retain the table row; got {top:?}"
    );
}

#[test]
fn ten_successful_file_reads_occupy_ten_transcript_rows() {
    let messages = (0..10)
        .map(|index| Message {
            kind: MessageKind::Tool {
                name: "read_file".into(),
                state: ToolState::Completed,
            },
            text: format!("完成\nfull file {index}\nmore body"),
            event_id: Some(format!("read:{index}")),
            summary: Some(format!("file{index}.rs · 返回 120 字节")),
        })
        .collect::<Vec<_>>();
    let rows = messages
        .iter()
        .flat_map(|message| message_lines(message, 78, false, false))
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 10);
    for (index, row) in rows.iter().enumerate() {
        assert!(
            row.spans
                .iter()
                .any(|span| span.content.contains(&format!("file{index}.rs")))
        );
    }
}

#[test]
fn overlay_borders_survive_terminal_diff_over_chinese_background() {
    use ratatui::{Terminal, backend::TestBackend};
    let state = bone::state::SessionState::new("/tmp/work");
    for (width, height) in [(80, 24), (120, 40)] {
        let mut view = View::new();
        view.push_message(Message {
            kind: MessageKind::Streaming,
            text: "修复过期".repeat(200),
            event_id: Some("background".into()),
            summary: None,
        });
        view.follow_conversation = false;
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| view.render(frame, &state, "m", "idle"))
            .unwrap();
        let sidebar = (width / 4).clamp(26, 32);
        assert_eq!(terminal.backend().buffer()[(sidebar + 1, 3)].symbol(), "修");
        view.open_detail("结果", "captured result");
        terminal
            .draw(|frame| view.render(frame, &state, "m", "idle"))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let top = (0..height)
            .find(|&row| buffer[(sidebar, row)].symbol() == "┌")
            .expect("overlay top corner is drawn");
        let bottom = (top + 1..height)
            .find(|&row| buffer[(sidebar, row)].symbol() == "└")
            .expect("overlay bottom corner is drawn");
        assert_eq!(buffer[(width - 1, top)].symbol(), "┐");
        for row in top + 1..bottom {
            assert_eq!(buffer[(sidebar, row)].symbol(), "│");
            assert_eq!(buffer[(width - 1, row)].symbol(), "│");
        }
    }
}

#[test]
fn transcript_keeps_original_paragraphs_without_person_role_headers() {
    let user = Message {
        kind: MessageKind::User { admitted: true },
        text: "请解释原因".into(),
        event_id: Some("user".into()),
        summary: None,
    };
    let answer = Message {
        kind: MessageKind::Delivery,
        text: "这是具体原因。".into(),
        event_id: Some("answer".into()),
        summary: None,
    };
    let user_rows = message_lines(&user, 78, false, false);
    let answer_rows = message_lines(&answer, 78, false, false);
    assert!(user_rows[0].spans[0].content.starts_with("│ "));
    let visible = user_rows
        .iter()
        .chain(&answer_rows)
        .flat_map(|line| &line.spans)
        .map(|span| span.content.as_ref())
        .collect::<String>();
    for hidden in ["你 ·", "Agent", "ab123456", "已纳入"] {
        assert!(!visible.contains(hidden));
    }
    assert!(visible.contains("请解释原因"));
    assert!(visible.contains("这是具体原因。"));
    let question = Message {
        kind: MessageKind::Question,
        text: "要使用哪个状态码？".into(),
        event_id: Some("q".into()),
        summary: None,
    };
    assert_eq!(
        message_lines(&question, 78, false, false)[0].spans[0].content,
        "提问"
    );
}

#[test]
fn composer_keeps_chinese_first_character_and_native_cursor_through_wrap_and_focus() {
    use ratatui::{Terminal, backend::TestBackend};
    let state = bone::state::SessionState::new("/tmp/work");
    for (width, height) in [(80, 24), (120, 40)] {
        let input_x = (width / 4).clamp(26, 32) + 1;
        let mut view = View::new();
        view.paste("首字中文");
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| view.render(frame, &state, "m", "暂无活动调用"))
            .unwrap();
        assert!(terminal.backend().cursor_visible());
        let position = terminal.backend().cursor_position();
        assert_eq!(position.x, input_x + 8);
        assert_eq!(
            terminal.backend().buffer()[(input_x, position.y)].symbol(),
            "首"
        );
        assert!(
            terminal.backend().buffer()[(position.x, position.y)]
                .modifier
                .contains(Modifier::REVERSED)
        );
        view.take_draft();
        view.paste("首中文字abc\n末尾");
        terminal
            .draw(|frame| view.render(frame, &state, "m", "暂无活动调用"))
            .unwrap();
        let draft = view.draft();
        let cursor = view.cursor();
        for (resize_width, resize_height) in [(12, 10), (width, height)] {
            terminal.backend_mut().resize(resize_width, resize_height);
            terminal
                .draw(|frame| view.render(frame, &state, "m", "暂无活动调用"))
                .unwrap();
            assert_eq!(view.draft(), draft);
            assert_eq!(view.cursor(), cursor);
        }
        let position = terminal.backend().cursor_position();
        assert_eq!(position.x, input_x + 4);
        assert_eq!(
            terminal.backend().buffer()[(input_x, position.y)].symbol(),
            "末"
        );
        assert_eq!(
            terminal.backend().buffer()[(input_x, position.y - 1)].symbol(),
            "首"
        );
        let first_row = (input_x..width)
            .map(|x| terminal.backend().buffer()[(x, position.y - 1)].symbol())
            .collect::<String>()
            .replace(' ', "");
        assert!(first_row.contains("首中文字abc"));
        view.handle_key(KeyEvent::new(
            KeyCode::Left,
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ));
        assert_eq!(view.selected_input_text().as_deref(), Some("尾"));
        let selected_cursor = view.cursor();
        for (resize_width, resize_height) in [(12, 10), (width, height)] {
            terminal.backend_mut().resize(resize_width, resize_height);
            terminal
                .draw(|frame| view.render(frame, &state, "m", "暂无活动调用"))
                .unwrap();
            assert_eq!(view.selected_input_text().as_deref(), Some("尾"));
            assert_eq!(view.cursor(), selected_cursor);
        }
        view.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));
        assert!(view.draft().is_empty());
        view.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::ALT));
        assert_eq!(view.draft(), draft);
        view.paste(&format!("\n{}\n末尾", "中".repeat(180)));
        terminal
            .draw(|frame| view.render(frame, &state, "m", "暂无活动调用"))
            .unwrap();
        let position = terminal.backend().cursor_position();
        assert_eq!(position.x, input_x + 4);
        assert_eq!(
            terminal.backend().buffer()[(input_x, position.y)].symbol(),
            "末"
        );
        assert_eq!(
            terminal.backend().buffer()[(input_x + 2, position.y)].symbol(),
            "尾"
        );
        let wrapped_cursor = view.cursor();
        for (resize_width, resize_height) in [(12, 10), (width, height)] {
            terminal.backend_mut().resize(resize_width, resize_height);
            terminal
                .draw(|frame| view.render(frame, &state, "m", "暂无活动调用"))
                .unwrap();
            assert_eq!(view.cursor(), wrapped_cursor);
        }
        let position = terminal.backend().cursor_position();
        assert_eq!(position.x, input_x + 4);
        assert_eq!(
            terminal.backend().buffer()[(input_x, position.y)].symbol(),
            "末"
        );
        assert_eq!(
            terminal.backend().buffer()[(input_x + 2, position.y)].symbol(),
            "尾"
        );
        view.handle_key(key(KeyCode::F(6)));
        terminal
            .draw(|frame| view.render(frame, &state, "m", "暂无活动调用"))
            .unwrap();
        assert!(!terminal.backend().cursor_visible());
        view.handle_key(key(KeyCode::F(6)));
        view.open_completion(
            PickerKind::Command,
            vec![PickerItem {
                label: "help".into(),
                detail: String::new(),
                value: "/help".into(),
            }],
            String::new(),
        );
        terminal
            .draw(|frame| view.render(frame, &state, "m", "暂无活动调用"))
            .unwrap();
        assert!(terminal.backend().cursor_visible());
        view.close_completion();
        view.open_help();
        terminal
            .draw(|frame| view.render(frame, &state, "m", "暂无活动调用"))
            .unwrap();
        assert!(!terminal.backend().cursor_visible());
    }
}

#[test]
fn running_action_and_receipt_remain_separate_while_input_stays_editable() {
    use ratatui::{Terminal, backend::TestBackend};
    let state = bone::state::SessionState::new("/tmp/work");
    let mut view = View::new();
    view.busy = true;
    view.live_status = "shell · cargo check · 12s".into();
    view.feedback_detail = "已发送 · 草稿已清空".into();
    view.reply_label = "新要求 · Enter 发送".into();
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal
        .draw(|frame| view.render(frame, &state, "m", "idle"))
        .unwrap();
    let initial = terminal
        .backend()
        .buffer()
        .content()
        .chunks(80)
        .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
        .collect::<Vec<_>>();
    let action_row = initial
        .iter()
        .position(|line| line.contains("cargo check"))
        .unwrap();
    let receipt_row = initial
        .iter()
        .position(|line| line.replace(' ', "").contains("草稿已清空"))
        .unwrap();
    assert_ne!(action_row, receipt_row);
    assert!(
        !initial
            .iter()
            .any(|line| line.replace(' ', "").contains("输入目标"))
    );
    assert!(initial[action_row].contains("12s"));
    view.spinner_tick = 1;
    view.paste("继续补充");
    terminal
        .draw(|frame| view.render(frame, &state, "m", "idle"))
        .unwrap();
    let next_action = terminal
        .backend()
        .buffer()
        .content()
        .chunks(80)
        .nth(action_row)
        .unwrap()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert_ne!(initial[action_row], next_action);
    assert_eq!(view.draft(), "继续补充");
    assert!(terminal.backend().cursor_visible());
    view.open_detail("结果", "captured raw");
    terminal
        .draw(|frame| view.render(frame, &state, "m", "idle"))
        .unwrap();
    let action = terminal
        .backend()
        .buffer()
        .content()
        .chunks(80)
        .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
        .find(|row| row.contains("cargo check"))
        .unwrap();
    assert!(action.contains("cargo check"));
    assert!(action.contains("12s"));
}

#[test]
fn session_focus_keeps_draft_selection_and_stable_session_identity() {
    use ratatui::{Terminal, backend::TestBackend};
    let state = bone::state::SessionState::new("/tmp/work");
    for (width, height) in [(80, 24), (120, 40)] {
        let mut view = View::new();
        let items = (0..30)
            .map(|index| PickerItem {
                label: format!("会话 {index:02}"),
                detail: "今天".into(),
                value: format!("session-{index}"),
            })
            .collect::<Vec<_>>();
        view.set_sessions(items.clone());
        view.mark_active_session("session-20");
        view.select_session("session-20");
        view.paste("保留中文abc");
        view.handle_key(KeyEvent::new(
            KeyCode::Left,
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ));
        let cursor = view.cursor();
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| view.render(frame, &state, "当前连接 · 原生模型", "idle"))
            .unwrap();
        assert!(terminal.backend().cursor_visible());
        assert_eq!(
            terminal.backend().buffer()[((width / 4).clamp(26, 32) - 1, 0)].symbol(),
            "│"
        );
        view.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT));
        view.handle_key(key(KeyCode::Down));
        assert_eq!(view.selected_session(), Some("session-21"));
        view.handle_paste("should not enter draft");
        assert_eq!(view.draft(), "保留中文abc");
        terminal
            .draw(|frame| view.render(frame, &state, "当前连接 · 原生模型", "idle"))
            .unwrap();
        assert!(!terminal.backend().cursor_visible());
        assert_eq!(terminal.backend().buffer()[(0, 0)].symbol(), " ");
        assert!(
            terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .any(|cell| cell.symbol() == "›")
        );
        let mut updated = items;
        updated.insert(
            0,
            PickerItem {
                label: "刚到达".into(),
                detail: String::new(),
                value: "new".into(),
            },
        );
        view.set_sessions(updated);
        assert_eq!(view.selected_session(), Some("session-21"));
        view.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT));
        assert_eq!(view.focus, Focus::Input);
        assert_eq!(view.cursor(), cursor);
        assert_eq!(view.selected_input_text().as_deref(), Some("c"));
        view.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));
        assert!(view.draft().is_empty());
        view.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::ALT));
        assert_eq!(view.draft(), "保留中文abc");
        view.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT));
        assert_eq!(view.focus, Focus::Conversation);
        view.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT));
        view.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT));
        assert_eq!(view.focus, Focus::Conversation);
        view.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT));
        assert_eq!(view.focus, Focus::Input);
    }
}

#[test]
fn narrow_session_sidebar_is_explicit_and_returns_to_original_reader() {
    use ratatui::{Terminal, backend::TestBackend};
    let state = bone::state::SessionState::new("/tmp/work");
    let mut view = View::new();
    view.set_sessions(vec![PickerItem {
        label: "窄屏会话".into(),
        detail: String::new(),
        value: "selected".into(),
    }]);
    view.push_message(Message {
        kind: MessageKind::Delivery,
        text: "完整原文\n\n".repeat(100),
        event_id: Some("reader".into()),
        summary: None,
    });
    view.paste("独立草稿");
    view.focus = Focus::Conversation;
    view.follow_conversation = false;
    let mut terminal = Terminal::new(TestBackend::new(42, 24)).unwrap();
    terminal
        .draw(|frame| view.render(frame, &state, "m", "idle"))
        .unwrap();
    view.conversation_scroll = 20;
    assert!(view.conversation_scroll < view.conversation_max);
    let point = view.read_points[20].offset;
    view.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT));
    terminal
        .draw(|frame| view.render(frame, &state, "m", "idle"))
        .unwrap();
    assert_eq!(terminal.backend().buffer()[(0, 0)].symbol(), " ");
    assert!(
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .any(|cell| cell.symbol() == "›")
    );
    assert!(!terminal.backend().cursor_visible());
    for (width, height) in [(80, 24), (42, 24)] {
        terminal.backend_mut().resize(width, height);
        terminal
            .draw(|frame| view.render(frame, &state, "m", "idle"))
            .unwrap();
    }
    view.handle_key(key(KeyCode::Esc));
    terminal
        .draw(|frame| view.render(frame, &state, "m", "idle"))
        .unwrap();
    assert_eq!(view.focus, Focus::Conversation);
    assert_eq!(view.read_points[view.conversation_scroll].offset, point);
    assert_eq!(view.draft(), "独立草稿");
    assert_eq!(view.sidebar_width, 0);
}

#[test]
fn connection_form_keeps_secrets_out_of_transcript_and_restores_parent_draft() {
    use ratatui::{Terminal, backend::TestBackend};
    let state = bone::state::SessionState::new("/tmp/work");
    let mut view = View::new();
    view.paste("原中文草稿");
    view.handle_key(KeyEvent::new(
        KeyCode::Left,
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ));
    let cursor = view.cursor();
    view.open_detail("原文", "完整记录");
    view.detail_scroll = 7;
    view.open_form(
        "连接",
        vec![
            FormField {
                label: "模型".into(),
                value: "native".into(),
                secret: false,
            },
            FormField {
                label: "密钥".into(),
                value: String::new(),
                secret: true,
            },
        ],
    );
    assert_eq!(view.form_step(), Some((0, 2)));
    view.handle_paste("-model");
    assert!(!view.advance_form());
    view.handle_paste("secret-token-123");
    view.form_error("密钥尚未验证");
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal
        .draw(|frame| view.render(frame, &state, "m", "idle"))
        .unwrap();
    let visible = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(!visible.contains("secret-token-123"));
    assert!(visible.contains('•'));
    assert!(visible.replace(' ', "").contains("密钥尚未验证"));
    assert!(terminal.backend().cursor_visible());
    assert_eq!(view.draft(), "原中文草稿");
    assert!(view.messages.is_empty());
    assert!(view.history().is_empty());
    view.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT));
    assert!(view.form_is_open());
    assert_ne!(view.focus, Focus::Sessions);
    view.open_help();
    view.handle_key(key(KeyCode::Esc));
    assert_eq!(
        view.form_values(),
        Some(vec!["native-model".into(), "secret-token-123".into()])
    );
    view.handle_key(key(KeyCode::BackTab));
    view.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));
    assert_eq!(view.form_values().unwrap()[0], "native");
    view.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::ALT));
    view.handle_key(key(KeyCode::Tab));
    assert!(view.advance_form());
    view.handle_key(key(KeyCode::Esc));
    assert!(!view.form_is_open());
    assert_eq!(
        view.detail.as_ref().map(|detail| detail.0.as_str()),
        Some("原文")
    );
    assert_eq!(view.detail_scroll, 7);
    view.handle_key(key(KeyCode::Esc));
    assert_eq!(view.cursor(), cursor);
    assert_eq!(view.selected_input_text().as_deref(), Some("稿"));
    view.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));
    assert!(view.draft().is_empty());
}

#[test]
fn workspace_shift_navigation_preserves_editor_and_stays_inside_layers() {
    let mut view = View::new();
    view.paste("alpha beta\n中文👩‍💻");
    let draft = view.draft();
    let end = view.cursor();
    view.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL));
    assert_eq!(view.focus, Focus::Input);
    assert!(view.cursor() < end);
    view.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL));
    assert_eq!(view.cursor(), end);
    view.handle_key(KeyEvent::new(
        KeyCode::Up,
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ));
    assert!(view.selected_input_text().is_some());
    assert_eq!(view.focus, Focus::Input);
    let cursor = view.cursor();
    let selection = view.selected_input_text();
    view.open_completion(PickerKind::Command, vec![], String::new());
    view.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT));
    assert!(!view.is_completion());
    assert_eq!(view.focus, Focus::Sessions);
    view.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT));
    assert_eq!(view.focus, Focus::Input);
    assert_eq!(
        (view.draft(), view.cursor(), view.selected_input_text()),
        (draft, cursor, selection)
    );
    view.open_help();
    view.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT));
    assert!(view.show_help);
    assert_eq!(view.focus, Focus::Input);
    view.handle_key(key(KeyCode::Esc));
    view.open_picker(PickerKind::Connection, vec![], String::new());
    view.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT));
    assert!(view.has_modal());
    assert_eq!(view.focus, Focus::Input);
    view.handle_key(key(KeyCode::Esc));
    view.toggle_inspector();
    view.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT));
    assert!(view.show_activity);
    assert_eq!(view.focus, Focus::Activity);
}

#[test]
fn session_rows_distinguish_current_browse_selection_and_keep_refresh_errors_visible() {
    use ratatui::{Terminal, backend::TestBackend};
    let state = bone::state::SessionState::new("/tmp/work");
    let mut view = View::new();
    let prefix = "同样前缀的很长软件工程任务，需要检查全部约束并修复";
    view.mark_active_session("a");
    view.set_sessions(vec![
        PickerItem {
            label: format!("{prefix} A"),
            detail: "运行".into(),
            value: "a".into(),
        },
        PickerItem {
            label: format!("{prefix} B"),
            detail: "回复".into(),
            value: "b".into(),
        },
    ]);
    view.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT));
    view.handle_key(key(KeyCode::Down));
    view.mark_active_session("a");
    view.sessions_loading = true;
    view.sessions_error = Some("读取失败 · 锁定".into());
    view.set_sessions(view.sessions.iter().cloned().rev().collect());
    assert_eq!(view.selected_session(), Some("b"));
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal
        .draw(|frame| view.render(frame, &state, "m", "idle"))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let row_text = |row| {
        (0..view.sidebar_width - 1)
            .map(|x| buffer[(x, row)].symbol())
            .collect::<String>()
    };
    assert!(row_text(1).replace(' ', "").contains("读取失败"));
    assert!(row_text(2).contains("B"));
    assert!(row_text(3).contains("A"));
    assert!(row_text(3).replace(' ', "").contains("运行"));
    assert!(row_text(2).replace(' ', "").contains("回复"));
    assert_eq!(buffer[(0, 0)].symbol(), " ");
    assert_eq!(buffer[(0, 2)].symbol(), "›");
    assert!(buffer[(0, 2)].modifier.contains(Modifier::REVERSED));
    // Wide glyph continuation cells are empty in TestBackend; assert the visible
    // glyphs themselves, including the rightmost attention word.
    assert!(
        (0..view.sidebar_width - 1)
            .filter(|&x| buffer[(x, 2)].symbol() != " ")
            .all(|x| buffer[(x, 2)].modifier.contains(Modifier::REVERSED))
    );
    assert!(!buffer[(0, 3)].modifier.contains(Modifier::REVERSED));
    assert_eq!(buffer[(1, 3)].symbol(), "•");
    assert!(buffer[(3, 3)].modifier.contains(Modifier::BOLD));
    for width in 0..35 {
        let fitted = middle_fit_line(&format!("{prefix} 👩‍💻 A"), width);
        assert!(UnicodeWidthStr::width(fitted.as_str()) <= width);
        if width >= 8 {
            assert!(fitted.ends_with(" A"));
        }
    }
}

#[test]
fn session_mouse_maps_single_line_viewport_and_ignores_hidden_or_modal_rows() {
    use crossterm::event::MouseButton;
    use ratatui::{Terminal, backend::TestBackend};
    let state = bone::state::SessionState::new("/tmp/work");
    let mut view = View::new();
    view.set_sessions(
        (0..30)
            .map(|index| PickerItem {
                label: format!("会话{index}"),
                detail: String::new(),
                value: index.to_string(),
            })
            .collect(),
    );
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal
        .draw(|frame| view.render(frame, &state, "m", "idle"))
        .unwrap();
    view.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT));
    view.handle_key(key(KeyCode::PageDown));
    assert_eq!(view.selected_session(), Some("21"));
    terminal
        .draw(|frame| view.render(frame, &state, "m", "idle"))
        .unwrap();
    assert_eq!(view.session_top, 1);
    let click = |row| MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 3,
        row,
        modifiers: KeyModifiers::NONE,
    };
    assert_eq!(view.clicked_session(click(2)), Some("1".into()));
    assert_eq!(view.clicked_session(click(5)), Some("4".into()));
    assert_eq!(view.clicked_session(click(0)), None);
    assert_eq!(view.clicked_session(click(23)), None);
    view.open_help();
    assert_eq!(view.clicked_session(click(2)), None);
    assert_eq!(view.selected_session(), Some("4"));
    view.handle_key(key(KeyCode::Esc));
    view.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT));
    terminal.backend_mut().resize(79, 24);
    terminal
        .draw(|frame| view.render(frame, &state, "m", "idle"))
        .unwrap();
    assert_eq!(view.sidebar_width, 0);
    assert_eq!(view.clicked_session(click(2)), None);
    view.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT));
    terminal
        .draw(|frame| view.render(frame, &state, "m", "idle"))
        .unwrap();
    assert_eq!(view.clicked_session(click(2)), Some("1".into()));
    terminal.backend_mut().resize(6, 8);
    terminal
        .draw(|frame| view.render(frame, &state, "m", "idle"))
        .unwrap();
    assert_eq!(view.clicked_session(click(2)), None);
}

#[test]
fn long_model_keeps_project_context_and_empty_state_teaches_only_in_the_composer() {
    use ratatui::{Terminal, backend::TestBackend};
    let state = bone::state::SessionState::new("/tmp/software-project");
    for width in [80, 120] {
        let mut view = View::new();
        let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
        terminal
            .draw(|frame| {
                view.render(
                    frame,
                    &state,
                    "团队订阅 · native/organization/very-long-configured-model-gpt-6-luna",
                    "等待输入",
                )
            })
            .unwrap();
        let rows = terminal
            .backend()
            .buffer()
            .content()
            .chunks(usize::from(width))
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>();
        assert!(rows[0].contains("software-project"));
        assert!(rows[1].replace(' ', "").contains("团队订阅"));
        assert!(rows[1].contains("6-luna"));
        let screen = rows.join("\n").replace(' ', "");
        assert_eq!(screen.matches("写下要完成的事").count(), 1);
        assert!(!screen.contains("在下方"));
        assert!(!screen.contains("输入中"));
        assert_ne!(view.composer_area, Rect::default());
        assert_ne!(view.conversation_area, Rect::default());
        view.focus = Focus::Sessions;
        terminal.backend_mut().resize(42, 24);
        terminal
            .draw(|frame| view.render(frame, &state, "m", "等待输入"))
            .unwrap();
        assert_eq!(view.composer_area, Rect::default());
        assert_eq!(view.conversation_area, Rect::default());
    }
}

#[test]
fn connection_save_reviews_public_values_with_one_action_hint_and_blocks_parent_hit_areas() {
    use ratatui::{Terminal, backend::TestBackend};
    let state = bone::state::SessionState::new("/tmp/work");
    for (width, height) in [(80, 24), (120, 40)] {
        let mut view = View::new();
        view.paste("保留我的草稿");
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| view.render(frame, &state, "m", "等待输入"))
            .unwrap();
        view.feedback_detail = "会话已打开 · 旧回执".into();
        view.open_form(
            "添加连接",
            vec![
                FormField {
                    label: "连接名称".into(),
                    value: "团队 API".into(),
                    secret: false,
                },
                FormField {
                    label: "模型名称".into(),
                    value: "gpt-6-luna".into(),
                    secret: false,
                },
                FormField {
                    label: "API endpoint".into(),
                    value: "http://localhost:9312/v1".into(),
                    secret: false,
                },
                FormField {
                    label: "API key".into(),
                    value: "secret-never-render-this".into(),
                    secret: true,
                },
            ],
        );
        for _ in 0..3 {
            view.handle_key(key(KeyCode::Tab));
        }
        terminal
            .draw(|frame| view.render(frame, &state, "m", "已暂停"))
            .unwrap();
        let screen = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>()
            .replace(' ', "");
        let rows = terminal
            .backend()
            .buffer()
            .content()
            .chunks(usize::from(width))
            .map(|row| {
                row.iter()
                    .map(|cell| cell.symbol())
                    .collect::<String>()
                    .replace(' ', "")
            })
            .collect::<Vec<_>>();
        let title_row = rows
            .iter()
            .position(|row| row.contains("添加连接"))
            .unwrap();
        let action_row = rows
            .iter()
            .position(|row| row.contains("Enter保存"))
            .unwrap();
        assert!(screen.contains("团队API"));
        assert!(screen.contains("gpt-6-luna"));
        assert!(screen.contains("http://localhost:9312/v1"));
        assert!(!screen.contains("secret-never-render-this"));
        assert!(!screen.contains("旧回执"));
        assert_eq!(screen.matches("Enter保存").count(), 1);
        assert!(
            action_row - title_row <= 8,
            "form should fit its content instead of filling the viewport"
        );
        assert_eq!(view.composer_area, Rect::default());
        assert_eq!(view.conversation_area, Rect::default());
        let draft_row = rows
            .iter()
            .position(|row| row.contains("保留我的草稿"))
            .unwrap();
        assert!(
            terminal.backend().buffer().content()
                [draft_row * usize::from(width)..(draft_row + 1) * usize::from(width)]
                .iter()
                .all(|cell| !cell.modifier.contains(Modifier::REVERSED)),
            "nonactive parent draft must not render a caret"
        );
        view.handle_key(key(KeyCode::Esc));
        assert_eq!(view.draft(), "保留我的草稿");
    }
}

#[test]
fn persisted_input_position_keeps_directed_multiline_grapheme_selection() {
    let text = "首行👩‍💻e\u{301}\n第二行完成";
    let start = "首行".len();
    let end = "首行👩‍💻e\u{301}\n第二".len();
    for (anchor, cursor) in [(start, end), (end, start)] {
        let mut original = View::new();
        original.paste(text);
        original.move_to_byte(anchor);
        original.editor.start_selection();
        original.move_to_byte(cursor);
        original.focus = Focus::Sessions;
        let saved = original.input_position();
        assert_eq!(saved, (cursor, Some((start, end))));
        let mut restored = View::new();
        restored.paste(text);
        restored.restore_input_position(Some(saved.0), saved.1);
        assert_eq!(
            restored.selected_input_text().as_deref(),
            Some("👩‍💻e\u{301}\n第二")
        );
        assert_eq!(restored.cursor(), cursor);
        original.focus = Focus::Input;
        let extend = KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL | KeyModifiers::SHIFT);
        original.handle_key(extend);
        restored.handle_key(extend);
        assert_eq!(restored.input_position(), original.input_position());
        assert_eq!(restored.draft(), text);
    }
}

#[test]
fn persisted_input_position_rejects_invalid_ranges_and_aligns_caret_safely() {
    let text = "中👩‍💻e\u{301}\n文";
    let emoji = "中".len();
    let combining = "中👩‍💻".len();
    let cases = [
        (1, Some((0, 1)), 0),
        (emoji + '👩'.len_utf8(), Some((0, emoji)), emoji),
        (combining + 1, Some((combining, text.len())), combining),
        (usize::MAX, Some((0, text.len())), text.len()),
        (emoji, Some((emoji, usize::MAX)), emoji),
        (emoji, Some((0, combining)), emoji),
        (emoji, Some((combining, emoji)), emoji),
        (emoji, Some((emoji, emoji)), emoji),
    ];
    for (cursor, selection, safe) in cases {
        let mut view = View::new();
        view.paste(text);
        view.restore_input_position(Some(cursor), selection);
        assert_eq!(view.cursor(), safe);
        assert_eq!(view.selected_input_text(), None);
        assert_eq!(view.draft(), text);
    }
    let mut view = View::new();
    view.paste(text);
    view.restore_input_position(None, None);
    assert_eq!(view.input_position(), (text.len(), None));
}

#[test]
fn persisted_input_position_uses_original_draft_during_temporary_notes() {
    let mut view = View::new();
    view.paste("原稿中文👩‍💻e\u{301}");
    view.handle_key(KeyEvent::new(
        KeyCode::Left,
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ));
    let original = view.input_position();
    view.begin_temporary_draft();
    view.paste("核查备注");
    view.handle_key(key(KeyCode::Left));
    assert_ne!(view.cursor(), original.0);
    assert_eq!(view.input_position(), original);
    assert_eq!(
        view.temporary_draft_text().as_deref(),
        Some("原稿中文👩‍💻e\u{301}")
    );
    assert!(view.restore_temporary_draft());
    assert_eq!(view.input_position(), original);
}
