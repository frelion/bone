use std::io;

use crossterm::{
    event::{DisableBracketedPaste, EnableBracketedPaste},
    execute,
};
use ratatui::{DefaultTerminal, Frame};

pub(crate) struct TerminalSession {
    terminal: DefaultTerminal,
}

impl TerminalSession {
    pub(crate) fn enter() -> io::Result<Self> {
        let mut terminal = ratatui::try_init()?;

        if let Err(error) = execute!(terminal.backend_mut(), EnableBracketedPaste) {
            let _ = ratatui::try_restore();
            return Err(error);
        }

        Ok(Self { terminal })
    }

    pub(crate) fn draw(&mut self, render: impl FnOnce(&mut Frame)) -> io::Result<()> {
        self.terminal.draw(render).map(|_| ())
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = execute!(self.terminal.backend_mut(), DisableBracketedPaste);
        let _ = ratatui::try_restore();
    }
}
