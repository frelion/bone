use std::{
    hint::black_box,
    process::Command,
    time::{Duration, Instant},
};

use crate::{
    state::{Action, HISTORY_CACHE_BYTES, SessionNavRow, UiEvent, UiState, WorkspaceTarget},
    view,
};
use bone_app::{HistoryEntry, HistoryPage, SessionEvent, SessionId, SessionInfo, WorkspaceId};
use ratatui::{Terminal, backend::TestBackend};

const SESSION_COUNT: usize = 100;
const HISTORY_PER_SESSION: usize = 1_000;

fn history_entry(sequence: u64) -> HistoryEntry {
    HistoryEntry {
        sequence: bone_app::SessionSeq(sequence),
        occurred_at: sequence as i64,
        event: SessionEvent::InputCancelled {
            input: bone_app::InputId(sequence),
        },
    }
}

fn populated_state() -> UiState {
    let workspace = WorkspaceId::new();
    let mut state = UiState::default();
    state.workspace_label = Some("performance-fixture".into());
    for index in 0..SESSION_COUNT {
        let info = SessionInfo {
            id: SessionId::new(),
            workspace,
            title: format!("性能会话 {index:03}"),
            archived: false,
        };
        state
            .session_rows
            .push(SessionNavRow::provisional(info.clone()));
        state
            .session_ui
            .insert(info.id, crate::state::SessionUi::new(info.id, 1));
    }
    state.selected = state.session_rows.first().map(SessionNavRow::id);
    state.set_workspace_target(WorkspaceTarget::SessionTitle);
    state
}

fn load_app_shaped_history(state: &mut UiState) {
    let sessions = state
        .session_rows
        .iter()
        .map(SessionNavRow::id)
        .collect::<Vec<_>>();
    for session in sessions {
        let page = HistoryPage {
            items: (1..=HISTORY_PER_SESSION as u64)
                .map(history_entry)
                .collect(),
            next_cursor: bone_app::SessionSeq(HISTORY_PER_SESSION as u64),
            has_more: false,
        };
        black_box(crate::state::update(
            state,
            UiEvent::HistoryLoaded {
                session,
                generation: 1,
                page,
            },
        ));
    }
}

fn retained_history_bytes(state: &UiState) -> usize {
    state
        .session_ui
        .values()
        .map(|session| session.transcript.allocated_bytes())
        .sum()
}

#[test]
fn hundred_sessions_and_hundred_thousand_app_records_stay_bounded() {
    let mut state = populated_state();
    load_app_shaped_history(&mut state);

    assert_eq!(state.session_rows.len(), SESSION_COUNT);
    assert!(state.session_ui.values().all(|session| {
        session.transcript.entries().count() <= crate::state::HISTORY_CACHE_ITEMS
    }));
    assert!(retained_history_bytes(&state) <= HISTORY_CACHE_BYTES);

    for index in 0..2_000 {
        let session = state.session_rows[index % SESSION_COUNT].id();
        black_box(crate::state::update(
            &mut state,
            UiEvent::Action(Action::SelectSession(session)),
        ));
    }
    assert!(retained_history_bytes(&state) <= HISTORY_CACHE_BYTES);
}

fn render_sample(state: &UiState, width: u16, height: u16, samples: usize) -> Vec<Duration> {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    (0..samples)
        .map(|_| {
            let started = Instant::now();
            terminal
                .draw(|frame| {
                    black_box(view::render(frame, state));
                })
                .expect("render sample");
            started.elapsed()
        })
        .collect()
}

fn input_to_frame_sample(state: &mut UiState, samples: usize) -> Vec<Duration> {
    let backend = TestBackend::new(160, 50);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    (0..samples)
        .map(|index| {
            let started = Instant::now();
            black_box(crate::state::update(
                state,
                UiEvent::Action(if index % 2 == 0 {
                    Action::ScrollDown(1)
                } else {
                    Action::ScrollUp(1)
                }),
            ));
            terminal
                .draw(|frame| {
                    black_box(view::render(frame, state));
                })
                .expect("input-to-frame sample");
            started.elapsed()
        })
        .collect()
}

fn percentile(samples: &mut [Duration], percentile: f64) -> Duration {
    samples.sort_unstable();
    let index = ((samples.len() - 1) as f64 * percentile).round() as usize;
    samples[index]
}

fn report(label: &str, mut samples: Vec<Duration>) -> (Duration, Duration, Duration) {
    let p50 = percentile(&mut samples, 0.50);
    let p95 = percentile(&mut samples, 0.95);
    let max = *samples.last().expect("non-empty samples");
    println!("{label}: p50={p50:?} p95={p95:?} max={max:?}");
    (p50, p95, max)
}

fn command_output(program: &str, arguments: &[&str]) -> String {
    Command::new(program)
        .args(arguments)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_else(|| "unavailable".into())
}

fn rss_kib() -> Option<u64> {
    std::fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

#[test]
#[ignore = "manual release-mode wall-clock measurement; run with --release --ignored --nocapture"]
fn release_tui_performance_harness() {
    println!("machine: {}", command_output("uname", &["-a"]));
    println!(
        "rust: {}",
        command_output("rustc", &["--version", "--verbose"])
    );
    println!("commit: {}", command_output("git", &["rev-parse", "HEAD"]));
    println!(
        "worktree: {}",
        if command_output("git", &["status", "--porcelain"]).is_empty() {
            "clean"
        } else {
            "dirty"
        }
    );
    let rss_before = rss_kib();
    println!("rss_before_kib: {rss_before:?}");

    let mut state = populated_state();
    load_app_shaped_history(&mut state);
    let (_, render_120_p95, _) = report("render_120x40", render_sample(&state, 120, 40, 1_000));
    let (_, render_160_p95, _) = report("render_160x50", render_sample(&state, 160, 50, 1_000));
    let (_, input_p95, _) = report(
        "input_to_render_160x50",
        input_to_frame_sample(&mut state, 1_000),
    );
    assert!(render_120_p95 <= Duration::from_millis(16));
    assert!(render_160_p95 <= Duration::from_millis(16));
    assert!(input_p95 <= Duration::from_millis(50));

    let switch_started = Instant::now();
    for index in 0..10_000 {
        let session = state.session_rows[index % SESSION_COUNT].id();
        black_box(crate::state::update(
            &mut state,
            UiEvent::Action(Action::SelectSession(session)),
        ));
    }
    println!("switch_10k_total: {:?}", switch_started.elapsed());
    println!("history_cache_bytes: {}", retained_history_bytes(&state));
    let rss_after = rss_kib();
    println!("rss_after_kib: {rss_after:?}");
    println!(
        "rss_delta_kib: {:?}",
        rss_after
            .zip(rss_before)
            .map(|(after, before)| after as i64 - before as i64)
    );
    assert!(retained_history_bytes(&state) <= HISTORY_CACHE_BYTES);
}
