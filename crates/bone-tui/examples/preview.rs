//! Render a deterministic fixture through the production renderer.
//! cargo run -p bone-tui --example preview -- 160 40 > preview.svg
use bone_app::{
    HistoryEntry, InputId, JobRef, RequestId, RuntimeId, RuntimeState, SessionEvent, SessionId,
    SessionInfo, SessionSeq, SessionView, WorkspaceId,
};
use bone_tui::{
    state::{SessionStatus, SessionUi, UiState},
    view,
};
use ratatui::{Terminal, backend::TestBackend, style::Color};
use std::sync::Arc;
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
fn main() {
    let args: Vec<_> = std::env::args().collect();
    let w = args.get(1).and_then(|v| v.parse().ok()).unwrap_or(160);
    let h = args.get(2).and_then(|v| v.parse().ok()).unwrap_or(40);
    let workspace = WorkspaceId::new();
    let mut state = UiState::default();
    state.workspace = Some((workspace, "BONE".into()));
    state.model_label = Some("Worker · GPT-5.5".into());
    for title in [
        "草稿恢复",
        "Context engine",
        "修复启动错误",
        "API 超时处理",
        "工具权限",
        "会话存储",
    ] {
        state.sessions.push(SessionInfo {
            id: SessionId::new(),
            workspace,
            title: title.into(),
            archived: false,
        });
    }
    state
        .session_statuses
        .insert(state.sessions[1].id, SessionStatus::NeedsAttention);
    state
        .session_statuses
        .insert(state.sessions[2].id, SessionStatus::Draft);
    let info = state.sessions[0].clone();
    state.selected = Some(info.id);
    let mut ui = SessionUi::new(info.clone(), 1);
    ui.hydrated = true;
    ui.draft = "组合字符之后继续输入时，\n也保留原来的光标位置。".into();
    ui.draft_cursor = ui.draft.len();
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
    state.session_ui.insert(ui.info.id, ui);
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal
        .draw(|frame| {
            view::render(frame, &state);
        })
        .unwrap();
    let cursor = terminal.get_cursor_position().unwrap();
    println!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{}\" height=\"{}\"><rect width=\"100%\" height=\"100%\" fill=\"#101010\"/>",
        w * 9,
        h * 20
    );
    for y in 0..h {
        let mut x = 0;
        while x < w {
            let cell = &terminal.backend().buffer()[(x, y)];
            let cells = unicode_width::UnicodeWidthStr::width(cell.symbol()).max(1) as u16;
            println!(
                "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"20\" fill=\"{}\"/>",
                x * 9,
                y * 20,
                cells * 9,
                color(cell.bg)
            );
            x += cells;
        }
    }
    for y in 0..h {
        for x in 0..w {
            let cell = &terminal.backend().buffer()[(x, y)];
            if cell.symbol() != " " {
                println!(
                    "<text x=\"{}\" y=\"{}\" fill=\"{}\" font-family=\"Menlo,monospace\" font-size=\"14\">{}</text>",
                    x * 9,
                    y * 20 + 15,
                    color(cell.fg),
                    escaped(cell.symbol())
                );
            }
        }
    }
    {
        let pos = cursor;
        println!(
            "<rect x=\"{}\" y=\"{}\" width=\"9\" height=\"20\" fill=\"#ff9d24\"/>",
            pos.x * 9,
            pos.y * 20
        );
    }
    println!("</svg>");
}
