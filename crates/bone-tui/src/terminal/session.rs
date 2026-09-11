use std::{
    io, panic,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use ratatui::{Terminal, backend::CrosstermBackend};
use tokio::sync::Notify;

use super::{capabilities::TerminalCapabilities, modes::ModeLease};

pub(crate) type TuiTerminal = Terminal<CrosstermBackend<io::Stdout>>;

type PanicHook = dyn for<'a> Fn(&panic::PanicHookInfo<'a>) + Send + Sync + 'static;

/// A process-wide panic is fatal to the active terminal session.
///
/// The panic hook only flips this atomic flag and wakes the runner. It never
/// locks a terminal mutex or performs I/O while another thread may be
/// unwinding. A panic on the owner task is cleaned up by `Drop`; a panic in a
/// detached task wakes the owner so it can stop drawing and restore normally.
#[derive(Clone)]
pub(crate) struct PanicSignal {
    tripped: Arc<AtomicBool>,
    wake: Arc<Notify>,
}

impl PanicSignal {
    pub(crate) fn new() -> Self {
        Self {
            tripped: Arc::new(AtomicBool::new(false)),
            wake: Arc::new(Notify::new()),
        }
    }

    pub(crate) fn trip(&self) {
        self.tripped.store(true, Ordering::Release);
        self.wake.notify_one();
    }

    pub(crate) fn is_tripped(&self) -> bool {
        self.tripped.load(Ordering::Acquire)
    }

    pub(crate) async fn notified(&self) {
        if !self.is_tripped() {
            self.wake.notified().await;
        }
    }
}

/// Owns the renderer and every temporary terminal mode used by BONE.
pub(crate) struct TerminalSession {
    terminal: Option<TuiTerminal>,
    modes: ModeLease,
    previous_hook: Option<Arc<PanicHook>>,
    panic_signal: PanicSignal,
    capabilities: TerminalCapabilities,
}

impl TerminalSession {
    pub(crate) fn enter() -> io::Result<Self> {
        // `ModeLease` restores itself if any later initialization step fails.
        let (modes, capabilities) = ModeLease::enter()?;
        let terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
        let panic_signal = PanicSignal::new();
        let mut session = Self {
            terminal: Some(terminal),
            modes,
            previous_hook: None,
            panic_signal,
            capabilities,
        };
        session.install_panic_hook();
        Ok(session)
    }

    pub(crate) fn terminal(&mut self) -> &mut TuiTerminal {
        self.terminal
            .as_mut()
            .expect("terminal session remains active while borrowed")
    }

    pub(crate) fn capabilities(&self) -> &TerminalCapabilities {
        &self.capabilities
    }

    pub(crate) fn panic_signal(&self) -> PanicSignal {
        self.panic_signal.clone()
    }

    /// Restores the shell before application shutdown work can block or fail.
    pub(crate) fn restore(&mut self) -> io::Result<()> {
        self.terminal.take();
        let result = self.modes.restore();
        self.restore_panic_hook();
        result
    }

    /// Re-enter temporary modes after the process is continued from a
    /// suspension. Capability negotiation is repeated because the process may
    /// resume under a different terminal or multiplexer.
    pub(crate) fn resume(&mut self) -> io::Result<()> {
        debug_assert!(self.terminal.is_none());
        debug_assert!(self.previous_hook.is_none());
        let capabilities = self.modes.resume()?;
        let terminal = match Terminal::new(CrosstermBackend::new(io::stdout())) {
            Ok(terminal) => terminal,
            Err(error) => {
                let _ = self.modes.restore();
                return Err(error);
            }
        };
        self.terminal = Some(terminal);
        self.capabilities = capabilities;
        self.install_panic_hook();
        Ok(())
    }

    fn install_panic_hook(&mut self) {
        debug_assert!(self.previous_hook.is_none());
        let previous_hook: Arc<PanicHook> = Arc::from(panic::take_hook());
        let delegated_hook = Arc::clone(&previous_hook);
        let panic_signal = self.panic_signal.clone();
        panic::set_hook(Box::new(move |info| {
            panic_signal.trip();
            delegated_hook(info);
        }));
        self.previous_hook = Some(previous_hook);
    }

    fn restore_panic_hook(&mut self) {
        // `set_hook` panics while this thread is already unwinding. `Drop`
        // still restores the terminal modes; leave the active hook alone until
        // process exit rather than turning one panic into an abort.
        if std::thread::panicking() {
            self.previous_hook.take();
            return;
        }
        if let Some(previous) = self.previous_hook.take() {
            panic::set_hook(Box::new(move |info| previous(info)));
        }
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        self.terminal.take();
        let _ = self.modes.restore();
        self.restore_panic_hook();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_session_is_send_for_single_runner_ownership() {
        fn assert_send<T: Send>() {}
        assert_send::<TerminalSession>();
    }

    #[tokio::test]
    async fn panic_signal_remembers_a_trip_that_precedes_the_waiter() {
        let signal = PanicSignal::new();
        signal.trip();

        tokio::time::timeout(std::time::Duration::from_millis(50), signal.notified())
            .await
            .expect("a prior panic must not be lost");
        assert!(signal.is_tripped());
    }
}
