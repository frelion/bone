use std::{io, panic, sync::Arc};

use crossterm::{
    cursor::Show,
    event::{DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};

pub type TuiTerminal = Terminal<CrosstermBackend<io::Stdout>>;

/// Owns every terminal mode changed by BONE and restores it on all unwind paths.
pub struct TerminalGuard {
    terminal: Option<TuiTerminal>,
    previous_hook: Option<Arc<PanicHook>>,
}

type PanicHook = dyn for<'a> Fn(&panic::PanicHookInfo<'a>) + Send + Sync + 'static;

impl TerminalGuard {
    pub fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(
            stdout,
            EnterAlternateScreen,
            EnableMouseCapture,
            EnableBracketedPaste
        ) {
            let _ = restore_terminal();
            return Err(error);
        }

        let terminal = match Terminal::new(CrosstermBackend::new(stdout)) {
            Ok(terminal) => terminal,
            Err(error) => {
                let _ = restore_terminal();
                return Err(error);
            }
        };
        let previous_hook: Arc<PanicHook> = Arc::from(panic::take_hook());
        let panic_hook = Arc::clone(&previous_hook);
        panic::set_hook(Box::new(move |info| {
            let _ = restore_terminal();
            panic_hook(info);
        }));
        Ok(Self {
            terminal: Some(terminal),
            previous_hook: Some(previous_hook),
        })
    }

    pub fn terminal(&mut self) -> &mut TuiTerminal {
        self.terminal
            .as_mut()
            .expect("terminal guard remains active while borrowed")
    }

    pub fn suspend(&mut self) -> io::Result<()> {
        let result = restore_terminal();
        self.terminal = None;
        self.restore_panic_hook();
        result
    }

    fn restore_panic_hook(&mut self) {
        // `set_hook` panics while this thread is already unwinding. The active
        // hook has restored the terminal at that point, so dropping its saved
        // predecessor is safer than turning one panic into an abort.
        if std::thread::panicking() {
            self.previous_hook.take();
            return;
        }
        if let Some(previous) = self.previous_hook.take() {
            panic::set_hook(Box::new(move |info| previous(info)));
        }
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = restore_terminal();
        self.restore_panic_hook();
    }
}

fn restore_terminal() -> io::Result<()> {
    let raw_result = disable_raw_mode();
    let screen_result = execute!(
        io::stdout(),
        DisableBracketedPaste,
        DisableMouseCapture,
        LeaveAlternateScreen,
        Show
    );
    raw_result.and(screen_result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_guard_is_send_for_single_runner_ownership() {
        fn assert_send<T: Send>() {}
        assert_send::<TerminalGuard>();
    }
}
