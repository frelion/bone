//! Full-screen terminal frontend and event export for the shared Agent API.

#![forbid(unsafe_code)]

mod agent_projection;
mod app;
mod command_effects;
pub mod commands;
mod events;
mod runtime_driver;
mod session_controller;
mod terminal;
mod view;
mod workspace;

use std::io;

use self::app::{App, AppEvent};
use crate::{ModelSelectionError, SettingsError};
use bone_agent::{HandleError, StartError};
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures_util::StreamExt;
use ratatui::{
    layout::{Alignment, Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
};

pub use events::write_events;
pub use workspace::run_workspace;

/// Show the only safe first-run response when BONE cannot open its SQLite
/// store. This is deliberately a tiny, storage-free TUI: it can retry an
/// external repair, or exit, but it never creates a second store, resets a
/// database, or exposes raw paths/payloads on screen.
pub async fn run_storage_repair(reason: &'static str) -> Result<bool, TuiError> {
    let mut terminal = terminal::TerminalSession::enter()?;
    let mut input = EventStream::new();
    loop {
        terminal.draw(|frame| render_storage_repair(frame, reason))?;
        let Some(event) = input.next().await else {
            return Ok(false);
        };
        let event = event?;
        if let Some(retry) = storage_repair_action(&event) {
            return Ok(retry);
        }
    }
}

fn storage_repair_action(event: &Event) -> Option<bool> {
    let Event::Key(key) = event else {
        return None;
    };
    if key.kind != KeyEventKind::Press {
        return None;
    }
    match key.code {
        KeyCode::Char('r') | KeyCode::Char('R') | KeyCode::Enter => Some(true),
        KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => Some(false),
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => Some(false),
        _ => None,
    }
}

fn render_storage_repair(frame: &mut ratatui::Frame<'_>, reason: &str) {
    let area = frame.area();
    let vertical_margin = area.height.saturating_sub(13) / 2;
    let horizontal_margin = area.width.saturating_sub(72) / 2;
    let [_, content, _] = Layout::vertical([
        Constraint::Length(vertical_margin),
        Constraint::Length(
            area.height
                .saturating_sub(vertical_margin.saturating_mul(2)),
        ),
        Constraint::Length(vertical_margin),
    ])
    .areas(area);
    let [_, content, _] = Layout::horizontal([
        Constraint::Length(horizontal_margin),
        Constraint::Length(
            area.width
                .saturating_sub(horizontal_margin.saturating_mul(2)),
        ),
        Constraint::Length(horizontal_margin),
    ])
    .areas(content);

    let text = vec![
        Line::from(Span::styled(
            "BONE storage needs repair",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(reason),
        Line::from(""),
        Line::from("BONE did not reset, replace, or delete any local data."),
        Line::from("Repair the storage location or wait for another BONE process to finish."),
        Line::from(""),
        Line::from(vec![
            Span::styled("R / Enter", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" retry   "),
            Span::styled("Q / Esc", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" exit"),
        ]),
    ];
    let panel = Paragraph::new(text)
        .alignment(Alignment::Left)
        .wrap(Wrap { trim: false })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(Color::Yellow))
                .title(" Storage repair "),
        );
    frame.render_widget(panel, content);
}

#[derive(Debug, thiserror::Error)]
pub enum TuiError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Agent(#[from] HandleError),
    #[error(transparent)]
    Start(#[from] StartError),
    #[error(transparent)]
    Workspace(#[from] crate::WorkspaceApplicationError),
    #[error(transparent)]
    SessionStore(#[from] crate::SessionStoreError),
    #[error(transparent)]
    Settings(#[from] SettingsError),
    #[error(transparent)]
    Input(#[from] ModelSelectionError),
}

/// Route an effect outcome through the sole presentation-state writer.
///
/// Runtime, durable-session, and command modules may produce user-facing
/// feedback, but none may mutate `App` fields directly.
pub(super) fn report_notice(app: &mut App, message: impl Into<String>) {
    let _ = app.reduce(AppEvent::Notice {
        message: message.into(),
    });
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyEvent, KeyEventState};

    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        })
    }

    #[test]
    fn storage_repair_has_only_retry_or_safe_exit_actions() {
        assert_eq!(
            storage_repair_action(&key(KeyCode::Char('r'), KeyModifiers::NONE)),
            Some(true)
        );
        assert_eq!(
            storage_repair_action(&key(KeyCode::Enter, KeyModifiers::NONE)),
            Some(true)
        );
        assert_eq!(
            storage_repair_action(&key(KeyCode::Esc, KeyModifiers::NONE)),
            Some(false)
        );
        assert_eq!(
            storage_repair_action(&key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(false)
        );
        assert_eq!(
            storage_repair_action(&key(KeyCode::Char('x'), KeyModifiers::NONE)),
            None
        );
    }
}
