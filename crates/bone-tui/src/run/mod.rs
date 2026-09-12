mod launch;
pub(crate) mod models;
mod runtime;

use std::{
    collections::{BTreeSet, VecDeque},
    env,
    future::Future,
    io,
    time::Duration,
};

use bone_app::{App, AppOptions, AttentionItem, WorkspaceOverview};
use crossterm::event::Event;
use thiserror::Error;
use tokio::sync::mpsc;

use crate::{
    input::terminal_event,
    state::{Action, Effect, SessionNavRow, UiEvent, UiState, update},
    terminal::{PanicSignal, TerminalEvents, TerminalSession},
};

use self::{
    launch::LaunchOptions,
    runtime::{Runtime, SessionReady},
};

const DRAFT_FLUSH_TIMEOUT: Duration = Duration::from_secs(2);
const APP_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const CARET_PHASE: Duration = Duration::from_millis(500);

enum FlushWait<T> {
    Completed(T),
    TimedOut,
    Process(ProcessSignal),
    Panic,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProcessSignal {
    Terminate,
    #[cfg(unix)]
    Suspend,
}

#[derive(Debug, Error)]
pub enum RunError {
    #[error("{0}")]
    App(#[from] bone_app::Error),
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("{0}")]
    Usage(String),
    #[error("{0}")]
    Shutdown(String),
}

pub async fn run() -> Result<(), RunError> {
    let launch = LaunchOptions::parse(env::args_os().skip(1))?;
    let app_options = launch
        .data_dir
        .map_or_else(AppOptions::platform_default, |path| {
            Ok(AppOptions::new(path))
        })?;
    let app = App::open(app_options).await?;
    let workspace = app.open_workspace(launch.workspace).await?;
    let overview = app.workspace_overview(workspace.id).await?;
    let last_active = app.last_active_session(workspace.id).await?;
    let model_facts = models::ModelFacts::from(app.resolved_workspace_config(workspace.id).await?);
    let rows = summarize_overview(&overview);

    let (tx, mut rx) = mpsc::channel(256);
    let (ready_tx, mut ready_rx) = mpsc::channel::<SessionReady>(16);
    let mut runtime = Runtime::new(app, workspace.id, tx, ready_tx);
    let mut state = UiState::default();
    let label = workspace
        .root
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("workspace")
        .to_owned();
    let mut effects = VecDeque::from(update(
        &mut state,
        UiEvent::WorkspaceOpened {
            label,
            rows,
            last_active,
            model_facts,
        },
    ));

    // Install process signal handlers before changing terminal modes. Registering inside
    // the spawned forwarding tasks leaves a window where the OS default action
    // can terminate the process without allowing `TerminalSession` to restore.
    let mut process_signals = process_signals()?;
    let (mut terminal, mut events): (TerminalSession, TerminalEvents) = TerminalSession::enter()?;
    let panic_signal = terminal.panic_signal();
    state.terminal_capabilities = terminal.capabilities().clone();
    let viewport = terminal.terminal().size()?;
    effects.extend(update(
        &mut state,
        UiEvent::Resized {
            width: viewport.width,
            height: viewport.height,
        },
    ));
    #[cfg(debug_assertions)]
    schedule_test_background_panic();
    let mut overview_ticker = tokio::time::interval(Duration::from_secs(5));
    overview_ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Startup already loaded the authoritative overview.
    overview_ticker.tick().await;
    let mut draft_ticker = tokio::time::interval(Duration::from_millis(500));
    draft_ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    draft_ticker.tick().await;
    let caret_sleep = tokio::time::sleep(CARET_PHASE);
    tokio::pin!(caret_sleep);
    let mut frame_snapshot = None;
    let mut view_state = crate::ui::frame::ViewState::default();
    let mut shutdown = false;
    let mut termination_received = false;
    let mut panic_handled = false;
    let mut shutdown_error = None;

    'run: while !shutdown {
        if panic_signal.is_tripped() {
            begin_background_panic_shutdown(
                &mut terminal,
                &mut state,
                &mut effects,
                &mut termination_received,
                &mut panic_handled,
                &mut shutdown_error,
            );
        }
        if !termination_received {
            render_dirty(
                &mut terminal,
                &mut state,
                &mut view_state,
                &mut frame_snapshot,
            )?;
            if panic_signal.is_tripped() {
                begin_background_panic_shutdown(
                    &mut terminal,
                    &mut state,
                    &mut effects,
                    &mut termination_received,
                    &mut panic_handled,
                    &mut shutdown_error,
                );
            }
        }
        while let Some(effect) = effects.pop_front() {
            let applied = tokio::select! {
                result = runtime.apply(effect) => Some(result),
                _ = panic_signal.notified(), if !panic_handled => None,
            };
            if let Some(result) = applied {
                shutdown |= result;
            } else {
                // Dropping the in-flight effect future prevents a synchronous
                // persistence call from pinning the terminal after another
                // task has already panicked.
                begin_background_panic_shutdown(
                    &mut terminal,
                    &mut state,
                    &mut effects,
                    &mut termination_received,
                    &mut panic_handled,
                    &mut shutdown_error,
                );
            }
            if panic_signal.is_tripped() {
                begin_background_panic_shutdown(
                    &mut terminal,
                    &mut state,
                    &mut effects,
                    &mut termination_received,
                    &mut panic_handled,
                    &mut shutdown_error,
                );
            }
        }
        if shutdown {
            loop {
                let result = wait_for_draft_flush(
                    runtime.flush_drafts(&mut state),
                    &mut process_signals,
                    &panic_signal,
                    !panic_handled,
                    DRAFT_FLUSH_TIMEOUT,
                )
                .await;
                if panic_signal.is_tripped() && !panic_handled {
                    begin_background_panic_shutdown(
                        &mut terminal,
                        &mut state,
                        &mut effects,
                        &mut termination_received,
                        &mut panic_handled,
                        &mut shutdown_error,
                    );
                    // The selected flush future may have completed at the same
                    // instant as the panic notification. Retry once under the
                    // fatal-shutdown policy so no ready-branch race can swallow
                    // the panic.
                    continue;
                }
                match result {
                    FlushWait::Completed(Ok(())) => break 'run,
                    FlushWait::Completed(Err(_)) if !termination_received => {
                        shutdown = false;
                        state.quitting = false;
                        state.status =
                            Some("Your draft could not be saved; the session is still open".into());
                        state.dirty = true;
                        break;
                    }
                    FlushWait::Completed(Err(error)) => {
                        shutdown_error
                            .get_or_insert_with(|| format!("终止时草稿未能保存：{error}"));
                        break 'run;
                    }
                    FlushWait::TimedOut if !termination_received => {
                        shutdown = false;
                        state.quitting = false;
                        state.status =
                            Some("Draft saving timed out; the session is still open".into());
                        state.dirty = true;
                        break;
                    }
                    FlushWait::TimedOut => {
                        shutdown_error.get_or_insert_with(|| "终止时草稿保存超时".into());
                        break 'run;
                    }
                    FlushWait::Process(ProcessSignal::Terminate) if termination_received => {
                        shutdown_error
                            .get_or_insert_with(|| "收到第二次终止信号；已停止等待草稿保存".into());
                        break 'run;
                    }
                    FlushWait::Process(ProcessSignal::Terminate) => {
                        termination_received = true;
                        // A signal makes exit final. Restore before restarting the
                        // bounded flush so no await holds the user's terminal.
                        if let Err(error) = terminal.restore() {
                            record_shutdown_error(
                                &mut shutdown_error,
                                format!("Terminal cleanup after a signal failed: {error}"),
                            );
                        }
                        continue;
                    }
                    FlushWait::Panic => {
                        begin_background_panic_shutdown(
                            &mut terminal,
                            &mut state,
                            &mut effects,
                            &mut termination_received,
                            &mut panic_handled,
                            &mut shutdown_error,
                        );
                        // `flush_drafts` is idempotent. The interrupted future
                        // is retried after the terminal has been restored.
                        continue;
                    }
                    #[cfg(unix)]
                    FlushWait::Process(ProcessSignal::Suspend) => {
                        if termination_received {
                            suspend_process()?;
                        } else {
                            suspend_and_resume(
                                &mut terminal,
                                &mut events,
                                &panic_signal,
                                &mut state,
                                &mut frame_snapshot,
                            )?;
                        }
                        // The interrupted flush future was dropped. Retry it after
                        // continuation; draft persistence is idempotent.
                        continue;
                    }
                }
            }
        }

        let next = tokio::select! {
            event = events.next() => {
                if panic_signal.is_tripped() {
                    begin_background_panic_shutdown(
                        &mut terminal,
                        &mut state,
                        &mut effects,
                        &mut termination_received,
                        &mut panic_handled,
                        &mut shutdown_error,
                    );
                    None
                } else {
                    let event = require_terminal_event(event)?;
                    terminal_event(event, frame_snapshot.as_ref(), &state)
                }
            },
            event = rx.recv() => event,
            ready = ready_rx.recv() => {
                ready.and_then(|ready| runtime.accept_ready(ready, &state))
            },
            _ = overview_ticker.tick() => Some(UiEvent::RefreshOverviewRequested),
            _ = draft_ticker.tick() => Some(UiEvent::PersistDraftsRequested),
            _ = &mut caret_sleep, if state.blinking_caret_active() => Some(UiEvent::CaretBlink),
            _ = panic_signal.notified(), if !panic_handled => {
                begin_background_panic_shutdown(
                    &mut terminal,
                    &mut state,
                    &mut effects,
                    &mut termination_received,
                    &mut panic_handled,
                    &mut shutdown_error,
                );
                None
            },
            signal = process_signals.recv() => {
                match signal {
                    Some(ProcessSignal::Terminate) => {
                        termination_received = true;
                        // An OS termination request is final. Return the terminal to
                        // the shell before any draft or application shutdown await.
                        if let Err(error) = terminal.restore() {
                            record_shutdown_error(
                                &mut shutdown_error,
                                format!("Terminal cleanup after a signal failed: {error}"),
                            );
                        }
                        Some(UiEvent::Action(Action::Quit))
                    }
                    #[cfg(unix)]
                    Some(ProcessSignal::Suspend) => {
                        suspend_and_resume(
                            &mut terminal,
                            &mut events,
                            &panic_signal,
                            &mut state,
                            &mut frame_snapshot,
                        )?;
                        caret_sleep.as_mut().reset(tokio::time::Instant::now() + CARET_PHASE);
                        None
                    }
                    None => continue,
                }
            },
        };
        if let Some(event) = next {
            let caret_was_active = state.blinking_caret_active();
            let reset_caret = matches!(
                event,
                UiEvent::Action(_) | UiEvent::CaretBlink | UiEvent::Resized { .. }
            );
            runtime.accept_event(&event);
            effects.extend(update(&mut state, event));
            if reset_caret || !caret_was_active && state.blinking_caret_active() {
                caret_sleep
                    .as_mut()
                    .reset(tokio::time::Instant::now() + CARET_PHASE);
            }
        }
    }

    let restore_error = terminal.restore().err();
    // Joining the reader and opening the restore barrier closes the last race
    // where a detached panic and a completed flush become ready together just
    // before the loop exits.
    if panic_signal.is_tripped() && !panic_handled {
        record_shutdown_error(
            &mut shutdown_error,
            "A background task panicked; BONE stopped safely",
        );
    }
    let app_shutdown_error =
        match tokio::time::timeout(APP_SHUTDOWN_TIMEOUT, runtime.shutdown()).await {
            Ok(Ok(())) => None,
            Ok(Err(error)) => Some(RunError::App(error)),
            Err(_) => Some(RunError::Shutdown(
                "App shutdown timed out after terminal restoration".into(),
            )),
        };

    if let Some(mut error) = shutdown_error {
        if let Some(app_error) = app_shutdown_error {
            error.push_str(&format!("; app shutdown also failed: {app_error}"));
        }
        if let Some(restore_error) = restore_error {
            error.push_str(&format!(
                "; final terminal cleanup also failed: {restore_error}"
            ));
        }
        return Err(RunError::Shutdown(error));
    }
    if let Some(app_error) = app_shutdown_error {
        if let Some(restore_error) = restore_error {
            return Err(RunError::Shutdown(format!(
                "{app_error}; final terminal cleanup also failed: {restore_error}"
            )));
        }
        return Err(app_error);
    }
    if let Some(restore_error) = restore_error {
        return Err(RunError::Io(restore_error));
    }
    Ok(())
}

fn record_shutdown_error(target: &mut Option<String>, message: impl AsRef<str>) {
    let message = message.as_ref();
    if let Some(existing) = target {
        existing.push_str("; ");
        existing.push_str(message);
    } else {
        *target = Some(message.to_owned());
    }
}

fn begin_background_panic_shutdown(
    terminal: &mut TerminalSession,
    state: &mut UiState,
    effects: &mut VecDeque<Effect>,
    termination_received: &mut bool,
    panic_handled: &mut bool,
    shutdown_error: &mut Option<String>,
) {
    if *panic_handled {
        return;
    }
    *panic_handled = true;
    *termination_received = true;
    // A detached panic hook waits without touching terminal state. Finish any
    // synchronous draw, join the input reader, and restore modes here; the
    // restore barrier then lets the delegated hook print to the shell.
    let restore_error = terminal.restore().err();
    effects.clear();
    effects.extend(update(state, UiEvent::Action(Action::Quit)));
    record_shutdown_error(
        shutdown_error,
        "A background task panicked; BONE stopped safely",
    );
    if let Some(error) = restore_error {
        record_shutdown_error(
            shutdown_error,
            format!("Terminal cleanup after the panic also failed: {error}"),
        );
    }
}

#[cfg(debug_assertions)]
fn schedule_test_background_panic() {
    const DELAY: &str = "BONE_TUI_TEST_BACKGROUND_PANIC_AFTER_MS";
    let Some(delay) = env::var_os(DELAY).and_then(|value| value.to_str()?.parse::<u64>().ok())
    else {
        return;
    };
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(delay)).await;
        panic!("injected detached-task panic for terminal lifecycle testing");
    });
}

/// Draw every state change before accepting another input or runtime event.
///
/// Keeping the visible frame and its hit map together prevents a click from an
/// old panel being interpreted against a newer state.
fn render_dirty(
    terminal: &mut TerminalSession,
    state: &mut UiState,
    view_state: &mut crate::ui::frame::ViewState,
    frame_snapshot: &mut Option<crate::view::FrameSnapshot>,
) -> io::Result<()> {
    if !state.dirty {
        return Ok(());
    }
    let mut rendered = None;
    terminal.terminal().draw(|frame| {
        rendered = Some(crate::view::render_with_view_state(
            frame, state, view_state,
        ));
    })?;
    *frame_snapshot = rendered;
    if let Some(metrics) = frame_snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.transcript_metrics.clone())
        && !crate::state::retain_transcript(state, metrics)
        && let Some(snapshot) = frame_snapshot
    {
        snapshot.transcript_metrics = None;
    }
    state.dirty = false;
    Ok(())
}

fn summarize_overview(overview: &WorkspaceOverview) -> Vec<SessionNavRow> {
    let attention = overview
        .attention
        .iter()
        .map(|item| match item {
            AttentionItem::WaitingForUser { session, .. }
            | AttentionItem::UnresolvedWrite { session, .. } => *session,
        })
        .collect::<BTreeSet<_>>();
    overview
        .sessions
        .iter()
        .cloned()
        .map(|summary| SessionNavRow {
            needs_attention: attention.contains(&summary.session.id),
            summary,
        })
        .collect()
}

async fn wait_for_draft_flush<T>(
    flush: impl Future<Output = T>,
    process_signals: &mut mpsc::UnboundedReceiver<ProcessSignal>,
    panic_signal: &PanicSignal,
    watch_for_panic: bool,
    timeout: Duration,
) -> FlushWait<T> {
    tokio::pin!(flush);
    tokio::select! {
        result = &mut flush => FlushWait::Completed(result),
        _ = tokio::time::sleep(timeout) => FlushWait::TimedOut,
        _ = panic_signal.notified(), if watch_for_panic => FlushWait::Panic,
        signal = process_signals.recv() => {
            FlushWait::Process(signal.expect("process signal forwarders remain alive"))
        },
    }
}

fn process_signals() -> io::Result<mpsc::UnboundedReceiver<ProcessSignal>> {
    let (tx, rx) = mpsc::unbounded_channel();
    #[cfg(unix)]
    for (kind, event) in [
        (
            tokio::signal::unix::SignalKind::interrupt(),
            ProcessSignal::Terminate,
        ),
        (
            tokio::signal::unix::SignalKind::hangup(),
            ProcessSignal::Terminate,
        ),
        (
            tokio::signal::unix::SignalKind::terminate(),
            ProcessSignal::Terminate,
        ),
        (
            tokio::signal::unix::SignalKind::from_raw(signal_hook::consts::signal::SIGQUIT),
            ProcessSignal::Terminate,
        ),
        (
            tokio::signal::unix::SignalKind::from_raw(signal_hook::consts::signal::SIGTSTP),
            ProcessSignal::Suspend,
        ),
    ] {
        let mut signal = tokio::signal::unix::signal(kind)?;
        let signal_tx = tx.clone();
        tokio::spawn(async move {
            while signal.recv().await.is_some() {
                if signal_tx.send(event).is_err() {
                    break;
                }
            }
        });
    }
    #[cfg(unix)]
    drop(tx);
    #[cfg(windows)]
    {
        // Construct every listener synchronously, before `TerminalSession::enter`.
        // Tokio registers the process-wide console handler during construction;
        // spawning first would leave a startup window with the OS default action.
        let mut ctrl_c = tokio::signal::windows::ctrl_c()?;
        let mut ctrl_break = tokio::signal::windows::ctrl_break()?;
        let mut ctrl_close = tokio::signal::windows::ctrl_close()?;
        let mut ctrl_logoff = tokio::signal::windows::ctrl_logoff()?;
        let mut ctrl_shutdown = tokio::signal::windows::ctrl_shutdown()?;

        macro_rules! forward_termination {
            ($listener:ident) => {{
                let signal_tx = tx.clone();
                tokio::spawn(async move {
                    while $listener.recv().await.is_some() {
                        if signal_tx.send(ProcessSignal::Terminate).is_err() {
                            break;
                        }
                    }
                });
            }};
        }

        forward_termination!(ctrl_c);
        forward_termination!(ctrl_break);
        forward_termination!(ctrl_close);
        forward_termination!(ctrl_logoff);
        forward_termination!(ctrl_shutdown);
        drop(tx);
    }
    Ok(rx)
}

fn require_terminal_event(event: Option<io::Result<Event>>) -> io::Result<Event> {
    event.transpose()?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "terminal input channel closed",
        )
    })
}

#[cfg(unix)]
fn suspend_process() -> io::Result<()> {
    // SIGSTOP cannot be intercepted, so the process is guaranteed to stop
    // after the terminal has been restored. The call returns after SIGCONT.
    signal_hook::low_level::raise(signal_hook::consts::signal::SIGSTOP)
}

#[cfg(unix)]
fn suspend_and_resume(
    terminal: &mut TerminalSession,
    events: &mut TerminalEvents,
    panic_signal: &PanicSignal,
    state: &mut UiState,
    frame_snapshot: &mut Option<crate::view::FrameSnapshot>,
) -> io::Result<()> {
    terminal.suspend()?;
    events.discard();
    if panic_signal.is_tripped() {
        return Err(io::Error::other(
            "a background task panicked while the terminal was suspending",
        ));
    }
    suspend_process()?;
    if panic_signal.is_tripped() {
        return Err(io::Error::other(
            "a background task panicked while the terminal was suspended",
        ));
    }
    *events = terminal.resume()?;
    state.terminal_capabilities = terminal.capabilities().clone();
    let viewport = terminal.terminal().size()?;
    let _ = update(
        state,
        UiEvent::Resized {
            width: viewport.width,
            height: viewport.height,
        },
    );
    state.caret_visible = true;
    *frame_snapshot = None;
    state.dirty = true;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_input_channel_eof_is_an_error_instead_of_a_busy_loop() {
        let error = require_terminal_event(None).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[tokio::test]
    async fn second_termination_stops_waiting_for_a_stuck_draft_flush() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        tx.send(ProcessSignal::Terminate).unwrap();
        let result = wait_for_draft_flush(
            std::future::pending::<()>(),
            &mut rx,
            &PanicSignal::new(),
            true,
            Duration::from_secs(60),
        )
        .await;
        assert!(matches!(
            result,
            FlushWait::Process(ProcessSignal::Terminate)
        ));
    }

    #[tokio::test]
    async fn draft_flush_has_a_strict_deadline() {
        let (_tx, mut rx) = mpsc::unbounded_channel();
        let panic_signal = PanicSignal::new();
        let result = wait_for_draft_flush(
            std::future::pending::<()>(),
            &mut rx,
            &panic_signal,
            true,
            Duration::from_millis(1),
        )
        .await;
        assert!(matches!(result, FlushWait::TimedOut));
    }

    #[tokio::test]
    async fn detached_panic_interrupts_a_pending_draft_flush() {
        let (_tx, mut rx) = mpsc::unbounded_channel();
        let panic_signal = PanicSignal::new();
        panic_signal.trip();

        let result = wait_for_draft_flush(
            std::future::pending::<()>(),
            &mut rx,
            &panic_signal,
            true,
            Duration::from_secs(60),
        )
        .await;

        assert!(matches!(result, FlushWait::Panic));
    }
}
