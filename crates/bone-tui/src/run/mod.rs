mod input;
mod launch;
mod runtime;

use std::{collections::VecDeque, env, future::Future, io, time::Duration};

use bone_app::{App, AppOptions};
use crossterm::event::EventStream;
use futures_util::StreamExt;
use thiserror::Error;
use tokio::sync::mpsc;

use crate::{
    state::{Action, UiEvent, UiState, update},
    terminal::TerminalGuard,
};

use self::{
    input::terminal_event,
    launch::LaunchOptions,
    runtime::{Runtime, SessionReady},
};

const FRAME_INTERVAL: Duration = Duration::from_millis(34);
const DRAFT_FLUSH_TIMEOUT: Duration = Duration::from_secs(2);
const APP_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

enum FlushWait<T> {
    Completed(T),
    TimedOut,
    Forced,
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
    let sessions = overview
        .sessions
        .iter()
        .map(|summary| summary.session.clone())
        .collect();

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
            attention: overview.attention,
            attention_projection_pending: overview.attention_projection_pending,
            unresolved_writes: overview.unresolved_writes,
        },
    ));

    // Install Unix handlers before changing terminal modes. Registering inside
    // the spawned forwarding tasks leaves a window where the OS default action
    // can terminate the process without allowing `TerminalGuard` to restore.
    let mut terminations = termination_events()?;
    let mut terminal = TerminalGuard::enter()?;
    let mut events = EventStream::new();
    let mut ticker = tokio::time::interval(FRAME_INTERVAL);
    let mut overview_ticker = tokio::time::interval(Duration::from_secs(5));
    overview_ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Startup already loaded the authoritative overview.
    overview_ticker.tick().await;
    let mut draft_ticker = tokio::time::interval(Duration::from_millis(500));
    draft_ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    draft_ticker.tick().await;
    let mut layout = None;
    let mut shutdown = false;
    let mut termination_received = false;
    let mut shutdown_error = None;

    while !shutdown {
        while let Some(effect) = effects.pop_front() {
            shutdown |= runtime.apply(effect).await?;
        }
        if shutdown {
            let result = wait_for_draft_flush(
                runtime.flush_drafts(&state),
                &mut terminations,
                termination_received,
                DRAFT_FLUSH_TIMEOUT,
            )
            .await;
            match result {
                FlushWait::Completed(Ok(())) => break,
                FlushWait::Completed(Err(error)) if !termination_received => {
                    shutdown = false;
                    state.quitting = false;
                    state.dialog = Some(crate::state::Dialog::Error(format!(
                        "草稿未能保存，BONE 仍保持打开：{error}"
                    )));
                    state.dirty = true;
                }
                FlushWait::Completed(Err(error)) => {
                    shutdown_error = Some(format!("终止时草稿未能保存：{error}"));
                    break;
                }
                FlushWait::TimedOut if !termination_received => {
                    shutdown = false;
                    state.quitting = false;
                    state.dialog = Some(crate::state::Dialog::Error(
                        "草稿保存超时，BONE 仍保持打开；可重试退出".into(),
                    ));
                    state.dirty = true;
                }
                FlushWait::TimedOut => {
                    shutdown_error = Some("终止时草稿保存超时".into());
                    break;
                }
                FlushWait::Forced => {
                    shutdown_error = Some("收到第二次终止信号；已停止等待草稿保存".into());
                    break;
                }
            }
        }

        let (next, frame_due) = tokio::select! {
            event = events.next() => (
                event.transpose()?.map(|event| terminal_event(event, layout.as_ref(), &state)),
                false
            ),
            event = rx.recv() => (event, false),
            ready = ready_rx.recv() => (
                ready.and_then(|ready| runtime.accept_ready(ready, &state)),
                false
            ),
            _ = ticker.tick() => (None, true),
            _ = overview_ticker.tick() => (Some(UiEvent::RefreshOverviewRequested), false),
            _ = draft_ticker.tick() => (Some(UiEvent::PersistDraftsRequested), false),
            signal = terminations.recv() => {
                if signal.is_none() {
                    continue;
                }
                termination_received = true;
                (Some(UiEvent::Action(Action::Terminate)), false)
            },
        };
        if let Some(event) = next {
            runtime.accept_event(&event, &state);
            effects.extend(update(&mut state, event));
        }
        if frame_due && state.dirty {
            terminal.terminal().draw(|frame| {
                layout = Some(crate::view::render(frame, &state));
            })?;
            state.dirty = false;
        }
    }

    terminal.suspend()?;
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

async fn wait_for_draft_flush<T>(
    flush: impl Future<Output = T>,
    terminations: &mut mpsc::UnboundedReceiver<()>,
    allow_force: bool,
    timeout: Duration,
) -> FlushWait<T> {
    tokio::pin!(flush);
    tokio::select! {
        result = &mut flush => FlushWait::Completed(result),
        _ = tokio::time::sleep(timeout) => FlushWait::TimedOut,
        _ = terminations.recv(), if allow_force => FlushWait::Forced,
    }
}

fn termination_events() -> io::Result<mpsc::UnboundedReceiver<()>> {
    let (tx, rx) = mpsc::unbounded_channel();
    #[cfg(unix)]
    for kind in [
        tokio::signal::unix::SignalKind::interrupt(),
        tokio::signal::unix::SignalKind::hangup(),
        tokio::signal::unix::SignalKind::terminate(),
    ] {
        let mut signal = tokio::signal::unix::signal(kind)?;
        let signal_tx = tx.clone();
        tokio::spawn(async move {
            while signal.recv().await.is_some() {
                if signal_tx.send(()).is_err() {
                    break;
                }
            }
        });
    }
    #[cfg(unix)]
    drop(tx);
    #[cfg(not(unix))]
    tokio::spawn(async move {
        std::future::pending::<()>().await;
        drop(tx);
    });
    Ok(rx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn second_termination_stops_waiting_for_a_stuck_draft_flush() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        tx.send(()).unwrap();
        let result = wait_for_draft_flush(
            std::future::pending::<()>(),
            &mut rx,
            true,
            Duration::from_secs(60),
        )
        .await;
        assert!(matches!(result, FlushWait::Forced));
    }

    #[tokio::test]
    async fn draft_flush_has_a_strict_deadline() {
        let (_tx, mut rx) = mpsc::unbounded_channel();
        let result = wait_for_draft_flush(
            std::future::pending::<()>(),
            &mut rx,
            false,
            Duration::from_millis(1),
        )
        .await;
        assert!(matches!(result, FlushWait::TimedOut));
    }
}
