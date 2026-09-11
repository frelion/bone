//! Render a deterministic fixture through the production renderer.
//! cargo run -p bone-tui --example preview -- 160 40 > preview.svg
use bone_app::{
    HistoryEntry, InputId, JobRef, RequestId, RuntimeId, RuntimeState, SessionEvent, SessionId,
    SessionInfo, SessionSeq, SessionSummary, SessionView, WorkspaceId,
};
use bone_tui::{
    state::{Action, Effect, Focus, SessionStatus, SessionUi, UiEvent, UiState, update},
    view,
};
use ratatui::{
    Terminal,
    backend::TestBackend,
    style::{Color, Modifier},
};
use std::{collections::BTreeMap, sync::Arc, time::SystemTime};
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
    let summaries = state
        .sessions
        .iter()
        .cloned()
        .zip(previews)
        .enumerate()
        .map(|(index, (session, (message_count, preview)))| {
            let summary = SessionSummary {
                session,
                created_at: now - i64::try_from(index + 1).unwrap() * 5 * 60_000,
                message_count,
                latest_reply_preview: preview.map(str::to_owned),
                projection_pending: false,
                has_draft: index == 2,
                draft_bytes: if index == 2 { 24 } else { 0 },
                persisted_runtime: None,
                history_through: SessionSeq(message_count),
            };
            (summary.session.id, summary)
        })
        .collect::<BTreeMap<_, _>>();
    let overview_sessions = state.sessions.clone();
    let overview_statuses = state.session_statuses.clone();
    update(
        &mut state,
        UiEvent::OverviewLoaded {
            generation: 0,
            sessions: overview_sessions,
            statuses: overview_statuses,
            summaries,
        },
    );
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
    let scenario = args.get(3).map(String::as_str).unwrap_or("conversation");
    if scenario == "commands" {
        let ui = state.selected_ui_mut().expect("preview session");
        ui.draft = "/".into();
        ui.draft_cursor = ui.draft.len();
    } else if matches!(scenario, "models" | "connection" | "form") {
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
        if matches!(scenario, "connection" | "form") {
            update(&mut state, UiEvent::Action(Action::ActivatePanel));
        }
        if scenario == "form" {
            update(&mut state, UiEvent::Action(Action::ActivatePanel));
        }
    }
    if scenario == "resized" {
        update(
            &mut state,
            UiEvent::Action(Action::BeginPaneResize(bone_tui::layout::PaneDivider::Left)),
        );
        update(
            &mut state,
            UiEvent::Action(Action::DragPane {
                widths: bone_tui::layout::PaneWidths {
                    left: 44,
                    right: 32,
                },
                finish: true,
            }),
        );
    }
    match scenario {
        "sessions" => {
            state.focus = Focus::Sessions;
            state.session_candidate = state.sessions.get(1).map(|session| session.id);
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
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal
        .draw(|frame| {
            view::render(frame, &state);
        })
        .unwrap();
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
                    "<text x=\"{}\" y=\"{}\" fill=\"{}\" font-family=\"DejaVu Sans Mono,WenQuanYi Zen Hei Mono,monospace\" font-size=\"14\" font-weight=\"{}\">{}</text>",
                    x * 9,
                    y * 20 + 15,
                    color(cell.fg),
                    if cell.modifier.contains(Modifier::BOLD) {
                        700
                    } else {
                        400
                    },
                    escaped(cell.symbol())
                );
            }
        }
    }
    println!("</svg>");
}
