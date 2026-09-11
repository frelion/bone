use std::{
    io,
    sync::{Arc, Mutex, TryLockError},
};

use crossterm::{
    cursor::Show,
    event::{DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};

use super::capabilities::TerminalCapabilities;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TerminalMode {
    Raw,
    AlternateScreen,
    MouseCapture,
    BracketedPaste,
    KeyboardEnhancement,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RestoreAction {
    Mode(TerminalMode),
    ShowCursor,
}

trait ModeBackend {
    fn enable(&mut self, mode: TerminalMode) -> io::Result<()>;
    fn disable(&mut self, mode: TerminalMode) -> io::Result<()>;
    fn show_cursor(&mut self) -> io::Result<()>;
}

/// Records every terminal mutation before it is attempted.
///
/// A terminal write can fail after sending part of an escape sequence, so a
/// failed enable still needs its matching restore action. Cleanup drains the
/// ledger in reverse order and keeps going after errors. Draining also makes a
/// second cleanup safe and silent.
struct ModeLedger<B> {
    backend: B,
    restore: Vec<RestoreAction>,
}

impl<B: ModeBackend> ModeLedger<B> {
    fn new(backend: B) -> Self {
        Self {
            backend,
            restore: Vec::new(),
        }
    }

    fn enable(&mut self, mode: TerminalMode) -> io::Result<()> {
        debug_assert!(
            !self.restore.contains(&RestoreAction::Mode(mode)),
            "a terminal mode must have one owner"
        );
        self.restore.push(RestoreAction::Mode(mode));
        self.backend.enable(mode)
    }

    fn restore_cursor_on_exit(&mut self) {
        debug_assert!(
            !self.restore.contains(&RestoreAction::ShowCursor),
            "cursor restoration must have one owner"
        );
        self.restore.push(RestoreAction::ShowCursor);
    }

    fn restore(&mut self) -> io::Result<()> {
        let mut first_error = None;
        let mut failed = Vec::new();
        while let Some(action) = self.restore.pop() {
            let result = match action {
                RestoreAction::Mode(mode) => self.backend.disable(mode),
                RestoreAction::ShowCursor => self.backend.show_cursor(),
            };
            if let Err(error) = result {
                if first_error.is_none() {
                    first_error = Some(error);
                }
                failed.push(action);
            }
        }
        // Keep only failed work. Reversing recreates the stack so a retry uses
        // the same cleanup order while successful actions remain idempotent.
        self.restore.extend(failed.into_iter().rev());
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

#[derive(Default)]
struct CrosstermModes;

impl ModeBackend for CrosstermModes {
    fn enable(&mut self, mode: TerminalMode) -> io::Result<()> {
        match mode {
            TerminalMode::Raw => enable_raw_mode(),
            TerminalMode::AlternateScreen => execute!(io::stdout(), EnterAlternateScreen),
            TerminalMode::MouseCapture => execute!(io::stdout(), EnableMouseCapture),
            TerminalMode::BracketedPaste => execute!(io::stdout(), EnableBracketedPaste),
            TerminalMode::KeyboardEnhancement => {
                #[cfg(unix)]
                {
                    execute!(
                        io::stdout(),
                        crossterm::event::PushKeyboardEnhancementFlags(
                            crossterm::event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                        )
                    )
                }
                #[cfg(not(unix))]
                {
                    Ok(())
                }
            }
        }
    }

    fn disable(&mut self, mode: TerminalMode) -> io::Result<()> {
        match mode {
            TerminalMode::Raw => disable_raw_mode(),
            TerminalMode::AlternateScreen => execute!(io::stdout(), LeaveAlternateScreen),
            TerminalMode::MouseCapture => execute!(io::stdout(), DisableMouseCapture),
            TerminalMode::BracketedPaste => execute!(io::stdout(), DisableBracketedPaste),
            TerminalMode::KeyboardEnhancement => {
                #[cfg(unix)]
                {
                    execute!(io::stdout(), crossterm::event::PopKeyboardEnhancementFlags)
                }
                #[cfg(not(unix))]
                {
                    Ok(())
                }
            }
        }
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        execute!(io::stdout(), Show)
    }
}

type SharedLedger = Arc<Mutex<ModeLedger<CrosstermModes>>>;

#[derive(Clone)]
pub(super) struct ModeRestorer {
    ledger: SharedLedger,
}

impl ModeRestorer {
    pub(super) fn restore(&self) -> io::Result<()> {
        let mut ledger = match self.ledger.try_lock() {
            Ok(ledger) => ledger,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "terminal mode cleanup is already in progress",
                ));
            }
        };
        ledger.restore()
    }
}

/// Owns the temporary terminal modes independently of the renderer.
///
/// Keeping this lease separate lets initialization failures clean themselves
/// up before a `TerminalSession` exists, while the shared ledger gives the
/// panic hook access to exactly the same idempotent restoration path.
pub(super) struct ModeLease {
    ledger: SharedLedger,
}

impl ModeLease {
    pub(super) fn enter() -> io::Result<(Self, TerminalCapabilities)> {
        let lease = Self {
            ledger: Arc::new(Mutex::new(ModeLedger::new(CrosstermModes))),
        };
        let capabilities = lease.acquire()?;
        Ok((lease, capabilities))
    }

    /// Reacquire the same temporary modes after a successful suspension
    /// restore. The shared ledger keeps the panic restorer valid.
    pub(super) fn resume(&self) -> io::Result<TerminalCapabilities> {
        self.acquire()
    }

    fn acquire(&self) -> io::Result<TerminalCapabilities> {
        self.enable(TerminalMode::Raw)?;
        self.enable(TerminalMode::AlternateScreen)?;
        self.enable(TerminalMode::MouseCapture)?;
        self.enable(TerminalMode::BracketedPaste)?;

        // Probe before EventStream becomes the process's only input reader.
        // Crossterm preserves unrelated input while it waits for the protocol
        // reply, so keys typed during startup are replayed to the app.
        let capabilities = detect_capabilities();
        if capabilities.uses_keyboard_enhancement() {
            self.enable(TerminalMode::KeyboardEnhancement)?;
        }

        // Ratatui owns cursor visibility while drawing. We do not change its
        // color or shape, but always return visibility to the user's terminal.
        self.with_ledger(|ledger| ledger.restore_cursor_on_exit());
        Ok(capabilities)
    }

    pub(super) fn panic_restorer(&self) -> ModeRestorer {
        ModeRestorer {
            ledger: Arc::clone(&self.ledger),
        }
    }

    pub(super) fn restore(&self) -> io::Result<()> {
        restore_shared(&self.ledger)
    }

    fn enable(&self, mode: TerminalMode) -> io::Result<()> {
        self.with_ledger(|ledger| ledger.enable(mode))
    }

    fn with_ledger<T>(&self, operation: impl FnOnce(&mut ModeLedger<CrosstermModes>) -> T) -> T {
        let mut ledger = self
            .ledger
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        operation(&mut ledger)
    }
}

fn restore_shared(ledger: &SharedLedger) -> io::Result<()> {
    let mut ledger = ledger
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    ledger.restore()
}

impl Drop for ModeLease {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

#[cfg(windows)]
fn detect_capabilities() -> TerminalCapabilities {
    TerminalCapabilities::native_windows()
}

#[cfg(unix)]
fn detect_capabilities() -> TerminalCapabilities {
    match crossterm::terminal::supports_keyboard_enhancement() {
        Ok(true) => TerminalCapabilities::kitty(),
        Ok(false) => {
            TerminalCapabilities::compatibility("terminal did not report a modified-key protocol")
        }
        Err(error) => {
            TerminalCapabilities::compatibility(format!("keyboard protocol probe failed: {error}"))
        }
    }
}

#[cfg(not(any(unix, windows)))]
fn detect_capabilities() -> TerminalCapabilities {
    TerminalCapabilities::compatibility("this platform does not expose modified Enter events")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum Call {
        Enable(TerminalMode),
        Disable(TerminalMode),
        ShowCursor,
    }

    #[derive(Default)]
    struct RecordingBackend {
        calls: Vec<Call>,
        enable_failure: Option<TerminalMode>,
        disable_failures: Vec<TerminalMode>,
        cursor_failure: bool,
    }

    impl ModeBackend for RecordingBackend {
        fn enable(&mut self, mode: TerminalMode) -> io::Result<()> {
            self.calls.push(Call::Enable(mode));
            if self.enable_failure == Some(mode) {
                Err(io::Error::other(format!("could not enable {mode:?}")))
            } else {
                Ok(())
            }
        }

        fn disable(&mut self, mode: TerminalMode) -> io::Result<()> {
            self.calls.push(Call::Disable(mode));
            if self.disable_failures.contains(&mode) {
                Err(io::Error::other(format!("could not disable {mode:?}")))
            } else {
                Ok(())
            }
        }

        fn show_cursor(&mut self) -> io::Result<()> {
            self.calls.push(Call::ShowCursor);
            if self.cursor_failure {
                Err(io::Error::other("could not show cursor"))
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn restoration_runs_in_reverse_acquisition_order() {
        let mut ledger = ModeLedger::new(RecordingBackend::default());
        ledger.enable(TerminalMode::Raw).unwrap();
        ledger.enable(TerminalMode::AlternateScreen).unwrap();
        ledger.enable(TerminalMode::BracketedPaste).unwrap();
        ledger.restore_cursor_on_exit();

        ledger.restore().unwrap();

        assert_eq!(
            ledger.backend.calls,
            [
                Call::Enable(TerminalMode::Raw),
                Call::Enable(TerminalMode::AlternateScreen),
                Call::Enable(TerminalMode::BracketedPaste),
                Call::ShowCursor,
                Call::Disable(TerminalMode::BracketedPaste),
                Call::Disable(TerminalMode::AlternateScreen),
                Call::Disable(TerminalMode::Raw),
            ]
        );
    }

    #[test]
    fn failed_enable_is_still_compensated() {
        let backend = RecordingBackend {
            enable_failure: Some(TerminalMode::MouseCapture),
            ..RecordingBackend::default()
        };
        let mut ledger = ModeLedger::new(backend);
        ledger.enable(TerminalMode::Raw).unwrap();
        ledger.enable(TerminalMode::AlternateScreen).unwrap();

        assert!(ledger.enable(TerminalMode::MouseCapture).is_err());
        ledger.restore().unwrap();

        assert_eq!(
            ledger.backend.calls,
            [
                Call::Enable(TerminalMode::Raw),
                Call::Enable(TerminalMode::AlternateScreen),
                Call::Enable(TerminalMode::MouseCapture),
                Call::Disable(TerminalMode::MouseCapture),
                Call::Disable(TerminalMode::AlternateScreen),
                Call::Disable(TerminalMode::Raw),
            ]
        );
    }

    #[test]
    fn cleanup_continues_after_errors_and_is_idempotent() {
        let backend = RecordingBackend {
            disable_failures: vec![TerminalMode::BracketedPaste],
            cursor_failure: true,
            ..RecordingBackend::default()
        };
        let mut ledger = ModeLedger::new(backend);
        ledger.enable(TerminalMode::Raw).unwrap();
        ledger.enable(TerminalMode::AlternateScreen).unwrap();
        ledger.enable(TerminalMode::BracketedPaste).unwrap();
        ledger.restore_cursor_on_exit();

        let error = ledger.restore().unwrap_err();
        assert_eq!(error.to_string(), "could not show cursor");
        let calls_after_first_restore = ledger.backend.calls.clone();

        ledger.backend.cursor_failure = false;
        ledger.backend.disable_failures.clear();
        ledger.restore().unwrap();
        assert!(calls_after_first_restore.ends_with(&[
            Call::ShowCursor,
            Call::Disable(TerminalMode::BracketedPaste),
            Call::Disable(TerminalMode::AlternateScreen),
            Call::Disable(TerminalMode::Raw),
        ]));
        assert!(ledger.backend.calls.ends_with(&[
            Call::ShowCursor,
            Call::Disable(TerminalMode::BracketedPaste),
        ]));
        let calls_after_retry = ledger.backend.calls.clone();
        ledger.restore().unwrap();
        assert_eq!(ledger.backend.calls, calls_after_retry);
    }

    #[test]
    fn panic_restorer_never_waits_on_a_busy_ledger() {
        let ledger = Arc::new(Mutex::new(ModeLedger::new(CrosstermModes)));
        let restorer = ModeRestorer {
            ledger: Arc::clone(&ledger),
        };
        let _busy = ledger.lock().unwrap();

        let error = restorer.restore().unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
    }
}
