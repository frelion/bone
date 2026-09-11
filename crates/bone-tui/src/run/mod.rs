mod launch;
pub(crate) mod models;
mod runtime;

use std::{collections::VecDeque, env, future::Future, io, time::Duration};

use bone_app::{App, AppOptions, AttentionItem, WorkspaceOverview};
use crossterm::event::{Event, EventStream};
use futures_util::StreamExt;
use thiserror::Error;
use tokio::sync::mpsc;

use crate::{
    input::terminal_event,
    state::{Action, SessionStatus, UiEvent, UiState, update},
    terminal::TerminalSession,
};

use self::{
    launch::LaunchOptions,
    runtime::{Runtime, SessionReady},
};

const DRAFT_FLUSH_TIMEOUT: Duration = Duration::from_secs(2);
const APP_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

enum FlushWait<T> {
    Completed(T),
    TimedOut,
    Process(ProcessSignal),
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
    let model_label = app
        .resolved_workspace_config(workspace.id)
        .await?
        .desired
        .ok()
        .map(|config| config.worker.selection.model);
    let (sessions, statuses) = summarize_overview(&overview);

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
            id: workspace.id,
            label,
            sessions,
            last_active,
            model_label,
            statuses,
        },
    ));

    // Install process signal handlers before changing terminal modes. Registering inside
    // the spawned forwarding tasks leaves a window where the OS default action
    // can terminate the process without allowing `TerminalSession` to restore.
    let mut process_signals = process_signals()?;
    let mut terminal = TerminalSession::enter()?;
    state.terminal_capabilities = terminal.capabilities().clone();
    let mut events = Some(EventStream::new());
    let mut overview_ticker = tokio::time::interval(Duration::from_secs(5));
    overview_ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Startup already loaded the authoritative overview.
    overview_ticker.tick().await;
    let mut draft_ticker = tokio::time::interval(Duration::from_millis(500));
    draft_ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    draft_ticker.tick().await;
    let mut frame_snapshot = None;
    let mut shutdown = false;
    let mut termination_received = false;
    let mut shutdown_error = None;

    'run: while !shutdown {
        if !termination_received {
            render_dirty(&mut terminal, &mut state, &mut frame_snapshot)?;
        }
        while let Some(effect) = effects.pop_front() {
            shutdown |= runtime.apply(effect).await?;
        }
        if shutdown {
            loop {
                let result = wait_for_draft_flush(
                    runtime.flush_drafts(&mut state),
                    &mut process_signals,
                    DRAFT_FLUSH_TIMEOUT,
                )
                .await;
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
                        shutdown_error = Some(format!("终止时草稿未能保存：{error}"));
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
                        shutdown_error = Some("终止时草稿保存超时".into());
                        break 'run;
                    }
                    FlushWait::Process(ProcessSignal::Terminate) if termination_received => {
                        shutdown_error = Some("收到第二次终止信号；已停止等待草稿保存".into());
                        break 'run;
                    }
                    FlushWait::Process(ProcessSignal::Terminate) => {
                        termination_received = true;
                        // A signal makes exit final. Restore before restarting the
                        // bounded flush so no await holds the user's terminal.
                        let _ = terminal.restore();
                        continue;
                    }
                    #[cfg(unix)]
                    FlushWait::Process(ProcessSignal::Suspend) => {
                        if termination_received {
                            suspend_process()?;
                        } else {
                            events.take();
                            suspend_and_resume(&mut terminal, &mut state, &mut frame_snapshot)?;
                            events = Some(EventStream::new());
                        }
                        // The interrupted flush future was dropped. Retry it after
                        // continuation; draft persistence is idempotent.
                        continue;
                    }
                }
            }
        }

        let next = tokio::select! {
            event = events
                .as_mut()
                .expect("terminal event stream is active")
                .next() => {
                let event = require_terminal_event(event)?;
                terminal_event(event, frame_snapshot.as_ref(), &state)
            },
            event = rx.recv() => event,
            ready = ready_rx.recv() => {
                ready.and_then(|ready| runtime.accept_ready(ready, &state))
            },
            _ = overview_ticker.tick() => Some(UiEvent::RefreshOverviewRequested),
            _ = draft_ticker.tick() => Some(UiEvent::PersistDraftsRequested),
            signal = process_signals.recv() => {
                match signal {
                    Some(ProcessSignal::Terminate) => {
                        termination_received = true;
                        // An OS termination request is final. Return the terminal to
                        // the shell before any draft or application shutdown await.
                        let _ = terminal.restore();
                        Some(UiEvent::Action(Action::Terminate))
                    }
                    #[cfg(unix)]
                    Some(ProcessSignal::Suspend) => {
                        events.take();
                        suspend_and_resume(
                            &mut terminal,
                            &mut state,
                            &mut frame_snapshot,
                        )?;
                        events = Some(EventStream::new());
                        None
                    }
                    None => continue,
                }
            },
        };
        if let Some(event) = next {
            runtime.accept_event(&event, &state);
            effects.extend(update(&mut state, event));
        }
    }

    terminal.restore()?;
    match tokio::time::timeout(APP_SHUTDOWN_TIMEOUT, runtime.shutdown()).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => return Err(error.into()),
        Err(_) => {
            return Err(RunError::Shutdown(
                "App shutdown timed out after terminal restoration".into(),
            ));
        }
    }
    if let Some(error) = shutdown_error {
        return Err(RunError::Shutdown(error));
    }
    Ok(())
}

/// Draw every state change before accepting another input or runtime event.
///
/// Keeping the visible frame and its hit map together prevents a click from an
/// old panel being interpreted against a newer state.
fn render_dirty(
    terminal: &mut TerminalSession,
    state: &mut UiState,
    frame_snapshot: &mut Option<crate::view::FrameSnapshot>,
) -> io::Result<()> {
    if !state.dirty {
        return Ok(());
    }
    terminal.terminal().draw(|frame| {
        *frame_snapshot = Some(crate::view::render(frame, state));
    })?;
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

fn summarize_overview(
    overview: &WorkspaceOverview,
) -> (
    Vec<bone_app::SessionInfo>,
    std::collections::BTreeMap<bone_app::SessionId, SessionStatus>,
) {
    let mut statuses = overview
        .sessions
        .iter()
        .map(|summary| {
            let status = if summary.persisted_runtime.is_some() {
                SessionStatus::Recoverable
            } else if summary.has_draft {
                SessionStatus::Draft
            } else {
                SessionStatus::Ready
            };
            (summary.session.id, status)
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    for item in &overview.attention {
        let session = match item {
            AttentionItem::WaitingForUser { session, .. }
            | AttentionItem::UnresolvedWrite { session, .. } => *session,
        };
        statuses.insert(session, SessionStatus::NeedsAttention);
    }
    let sessions = overview
        .sessions
        .iter()
        .map(|summary| summary.session.clone())
        .collect();
    (sessions, statuses)
}

async fn wait_for_draft_flush<T>(
    flush: impl Future<Output = T>,
    process_signals: &mut mpsc::UnboundedReceiver<ProcessSignal>,
    timeout: Duration,
) -> FlushWait<T> {
    tokio::pin!(flush);
    tokio::select! {
        result = &mut flush => FlushWait::Completed(result),
        _ = tokio::time::sleep(timeout) => FlushWait::TimedOut,
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
    #[cfg(not(any(unix, windows)))]
    tokio::spawn(async move {
        while tokio::signal::ctrl_c().await.is_ok() {
            if tx.send(ProcessSignal::Terminate).is_err() {
                break;
            }
        }
    });
    Ok(rx)
}

fn require_terminal_event(event: Option<io::Result<Event>>) -> io::Result<Event> {
    event
        .transpose()?
        .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "terminal event stream ended"))
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
    state: &mut UiState,
    frame_snapshot: &mut Option<crate::view::FrameSnapshot>,
) -> io::Result<()> {
    terminal.restore()?;
    suspend_process()?;
    terminal.resume()?;
    state.terminal_capabilities = terminal.capabilities().clone();
    *frame_snapshot = None;
    state.dirty = true;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_event_stream_eof_is_an_error_instead_of_a_busy_loop() {
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
        let result = wait_for_draft_flush(
            std::future::pending::<()>(),
            &mut rx,
            Duration::from_millis(1),
        )
        .await;
        assert!(matches!(result, FlushWait::TimedOut));
    }
}
