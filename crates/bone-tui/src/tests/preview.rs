//! Ignored artifact generator for deterministic production-renderer previews.
//!
//! The output path and terminal dimensions are required environment variables,
//! so the test never chooses an implicit destination in the worktree.
use std::{env, fmt::Write as _, fs, path::PathBuf, sync::Arc, time::SystemTime};

use crate::{
    state::{
        Action, EditCommand, EditorTarget, Effect, Focus, SessionNavRow, SessionUi, UiEvent,
        UiState, update,
    },
    view,
};
use bone_app::{
    HistoryEntry, InputId, JobRef, RequestId, RuntimeId, RuntimeState, SessionEvent, SessionId,
    SessionInfo, SessionSeq, SessionSummary, SessionView, WorkspaceId,
};
use ratatui::{
    Terminal,
    backend::TestBackend,
    style::{Color, Modifier},
};

const CELL_WIDTH: u16 = 9;
const CELL_HEIGHT: u16 = 20;

fn escaped(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
fn color(value: Color) -> String {
    match value {
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        _ => "#101010".into(),
    }
}

fn required_dimension(name: &str) -> u16 {
    env::var(name)
        .unwrap_or_else(|_| panic!("{name} must be set"))
        .parse()
        .unwrap_or_else(|_| panic!("{name} must be a positive u16"))
}

#[test]
#[ignore = "writes a requested SVG artifact; run explicitly with preview environment variables"]
fn render_preview_artifact() {
    let width = required_dimension("BONE_TUI_PREVIEW_WIDTH");
    let height = required_dimension("BONE_TUI_PREVIEW_HEIGHT");
    assert!(width > 0, "BONE_TUI_PREVIEW_WIDTH must be positive");
    assert!(height > 0, "BONE_TUI_PREVIEW_HEIGHT must be positive");
    let output = PathBuf::from(
        env::var_os("BONE_TUI_PREVIEW_OUTPUT")
            .expect("BONE_TUI_PREVIEW_OUTPUT must name the SVG artifact"),
    );
    let scenario =
        env::var("BONE_TUI_PREVIEW_SCENARIO").unwrap_or_else(|_| "conversation".to_owned());
    let workspace = WorkspaceId::new();
    let mut state = UiState::default();
    state.workspace = Some((workspace, "BONE".into()));
    state.model_label = Some("Worker · GPT-5.5".into());
    let sessions = [
        "草稿恢复",
        "Context engine",
        "修复启动错误",
        "API 超时处理",
        "工具权限",
        "会话存储",
    ]
    .into_iter()
    .map(|title| SessionInfo {
        id: SessionId::new(),
        workspace,
        title: title.into(),
        archived: false,
    })
    .collect::<Vec<_>>();
    let now = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
        });
    let previews = [
        (4, Some("我会把输入定位按实际字符宽度一起验证。")),
        (
            18,
            Some("The projection now resumes from its durable cursor."),
        ),
        (7, Some("已定位启动阶段的终端恢复顺序。")),
        (11, Some("Retry preserves the original request identity.")),
        (6, Some("权限确认只在真正需要时出现。")),
        (3, None),
    ];
    state.session_rows = sessions
        .into_iter()
        .zip(previews)
        .enumerate()
        .map(
            |(index, (session, (message_count, preview)))| SessionNavRow {
                summary: SessionSummary {
                    session,
                    created_at: now - i64::try_from(index + 1).unwrap() * 5 * 60_000,
                    message_count,
                    latest_reply_preview: preview.map(str::to_owned),
                    projection_pending: false,
                    has_draft: index == 2,
                    draft_bytes: if index == 2 { 24 } else { 0 },
                    persisted_runtime: None,
                    history_through: SessionSeq(message_count),
                },
                needs_attention: index == 1,
            },
        )
        .collect();
    let info = state.session_rows[0].info().clone();
    state.selected = Some(info.id);
    let mut ui = SessionUi::new(info.id, 1);
    ui.hydrated = true;
    ui.snapshot = Some(Arc::new(SessionView {
        session: info,
        runtime: RuntimeState::Detached,
        draft: String::new(),
        inputs: vec![],
        jobs: vec![],
        activity: vec![],
        history_through: SessionSeq(4),
        problem: None,
    }));
    let runtime = RuntimeId::new();
    let events = [
        SessionEvent::InputSubmitted { input: InputId(1), request_id: RequestId::new(), text: "切换会话时保留新草稿，补上回归测试。".into(), reply_to: None },
        SessionEvent::Reply { job: JobRef { runtime, id: 1 }, inputs: vec![InputId(1)], text: "已修复提交回执对新草稿的误清理。\n\n现在只有提交版本与当前草稿一致时，输入才会清空。\n会话切换会分别保留草稿、光标和阅读位置。\n\n```rust\nif receipt.revision == draft.revision {\n    draft.clear();\n}\n```".into() },
        SessionEvent::InputSubmitted { input: InputId(2), request_id: RequestId::new(), text: "再检查中文和组合字符，别让光标跳位。".into(), reply_to: None },
        SessionEvent::Reply { job: JobRef { runtime, id: 2 }, inputs: vec![InputId(2)], text: "我会把输入定位按实际字符宽度一起验证。".into() }
    ];
    for (i, event) in events.into_iter().enumerate() {
        ui.history.push_back(HistoryEntry {
            sequence: SessionSeq(i as u64 + 1),
            occurred_at: i as i64,
            event,
        });
    }
    if scenario == "long-reply" {
        for number in 1..=4 {
            ui.history.push_back(HistoryEntry {
                sequence: SessionSeq(ui.history.len() as u64 + 1),
                occurred_at: ui.history.len() as i64,
                event: SessionEvent::Reply {
                    job: JobRef {
                        runtime,
                        id: number + 2,
                    },
                    inputs: Vec::new(),
                    text: format!(
                        "## Finding {number}\n\nThe draft remains attached to its session.\n\n```rust\nlet draft = session.draft();\nsave(draft).await?;\n```\n\n- Preserve pending text\n- Restore the reading position\n"
                    ),
                },
            });
        }
    }
    state.session_ui.insert(ui.id, ui);
    update(
        &mut state,
        UiEvent::Action(Action::Edit {
            target: EditorTarget::Composer,
            command: EditCommand::Insert {
                text: "组合字符之后继续输入时，\n也保留原来的光标位置。".into(),
                typing: false,
            },
        }),
    );
    if scenario == "commands" {
        update(
            &mut state,
            UiEvent::Action(Action::Edit {
                target: EditorTarget::Composer,
                command: EditCommand::Clear,
            }),
        );
        update(
            &mut state,
            UiEvent::Action(Action::Edit {
                target: EditorTarget::Composer,
                command: EditCommand::Insert {
                    text: "/".into(),
                    typing: false,
                },
            }),
        );
    } else if matches!(scenario.as_str(), "models" | "connection" | "form") {
        let effects = update(&mut state, UiEvent::Action(Action::OpenModels));
        for effect in effects {
            if let Effect::LoadModels { session, request } = effect {
                update(
                    &mut state,
                    UiEvent::ModelsLoaded {
                        session,
                        request,
                        choices: vec![],
                        profiles: vec![],
                    },
                );
            }
        }
        if matches!(scenario.as_str(), "connection" | "form") {
            update(&mut state, UiEvent::Action(Action::ActivatePanel));
        }
        if scenario == "form" {
            update(&mut state, UiEvent::Action(Action::ActivatePanel));
        }
    }
    if scenario == "resized" {
        update(
            &mut state,
            UiEvent::Action(Action::BeginPaneResize(crate::layout::PaneDivider::Left)),
        );
        update(
            &mut state,
            UiEvent::Action(Action::DragPane {
                widths: crate::layout::PaneWidths {
                    left: 44,
                    right: 32,
                },
                finish: true,
            }),
        );
    }
    match scenario.as_str() {
        "sessions" => {
            state.focus = Focus::Sessions;
            state.session_candidate = state.session_rows.get(1).map(SessionNavRow::id);
        }
        "title" => {
            update(
                &mut state,
                UiEvent::Action(Action::Focus(Focus::SessionTitle)),
            );
        }
        "right" => state.focus = Focus::RightRail,
        _ => {}
    }
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| {
            view::render(frame, &state);
        })
        .unwrap();
    let mut svg = String::new();
    writeln!(
        svg,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{}\" height=\"{}\"><rect width=\"100%\" height=\"100%\" fill=\"#101010\"/>",
        width * CELL_WIDTH,
        height * CELL_HEIGHT
    )
    .unwrap();
    for y in 0..height {
        let mut x = 0;
        while x < width {
            let cell = &terminal.backend().buffer()[(x, y)];
            let cells = unicode_width::UnicodeWidthStr::width(cell.symbol()).max(1) as u16;
            writeln!(
                svg,
                "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"20\" fill=\"{}\"/>",
                x * CELL_WIDTH,
                y * CELL_HEIGHT,
                cells * CELL_WIDTH,
                color(cell.bg)
            )
            .unwrap();
            x += cells;
        }
    }
    for y in 0..height {
        for x in 0..width {
            let cell = &terminal.backend().buffer()[(x, y)];
            if cell.symbol() != " " {
                writeln!(
                    svg,
                    "<text x=\"{}\" y=\"{}\" fill=\"{}\" font-family=\"DejaVu Sans Mono,WenQuanYi Zen Hei Mono,monospace\" font-size=\"14\" font-weight=\"{}\">{}</text>",
                    x * CELL_WIDTH,
                    y * CELL_HEIGHT + 15,
                    color(cell.fg),
                    if cell.modifier.contains(Modifier::BOLD) {
                        700
                    } else {
                        400
                    },
                    escaped(cell.symbol())
                )
                .unwrap();
            }
        }
    }
    svg.push_str("</svg>\n");
    fs::write(&output, svg).expect("write preview SVG");
    println!("wrote {}", output.display());
}
