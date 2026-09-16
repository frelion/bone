use std::io;

#[cfg(unix)]
use crossterm::event::{
    DisableBracketedPaste, DisableFocusChange, EnableBracketedPaste, EnableFocusChange,
};
use crossterm::{
    cursor::Show,
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};

use super::{
    capabilities::TerminalCapabilities,
    output::{PointerShape, reset_pointer},
};

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
compile_error!("bone-tui supports Windows, Linux/WSL, and macOS");

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TerminalMode {
    Raw,
    AlternateScreen,
    MouseCapture,
    PointerShape,
    #[cfg(unix)]
    FocusChange,
    #[cfg(unix)]
    BracketedPaste,
    #[cfg(unix)]
    KeyboardEnhancement,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RestoreAction {
    Mode(TerminalMode),
    ShowCursor,
    /// Reapply the exact host console modes after all portable cleanup.
    ///
    /// Crossterm's native Windows raw-mode cleanup enables a fixed set of
    /// input flags, and its ANSI detection may enable VT output processing.
    /// Neither operation is an exact inverse of the state inherited from the
    /// parent process, so Windows keeps an explicit snapshot as the final
    /// cleanup barrier.
    HostConsole,
}

trait ModeBackend {
    fn enable(&mut self, mode: TerminalMode) -> io::Result<()>;
    fn disable(&mut self, mode: TerminalMode) -> io::Result<()>;
    fn show_cursor(&mut self) -> io::Result<()>;
    fn restore_host_console(&mut self) -> io::Result<()>;
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

    #[cfg(any(windows, test))]
    fn restore_host_console_on_exit(&mut self) {
        debug_assert!(
            !self.restore.contains(&RestoreAction::HostConsole),
            "the inherited console modes must have one owner"
        );
        // Register this before any portable mode. Reverse cleanup therefore
        // reapplies the inherited state only after every other compensation.
        self.restore.push(RestoreAction::HostConsole);
    }

    fn restore(&mut self) -> io::Result<()> {
        let mut first_error = None;
        let mut failed = Vec::new();
        while let Some(action) = self.restore.pop() {
            let result = match action {
                RestoreAction::Mode(mode) => self.backend.disable(mode),
                RestoreAction::ShowCursor => self.backend.show_cursor(),
                RestoreAction::HostConsole => self.backend.restore_host_console(),
            };
            if let Err(error) = result {
                if first_error.is_none() {
                    first_error = Some(error);
                }
                failed.push(action);
            } else if action == RestoreAction::HostConsole && !failed.is_empty() {
                // A later retry of an earlier compensation may mutate console
                // state again. Keep the exact snapshot as a final barrier until
                // every dependent cleanup action has succeeded.
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

struct CrosstermModes {
    #[cfg(windows)]
    inherited_console: WindowsConsoleModes,
}

impl CrosstermModes {
    fn capture() -> io::Result<Self> {
        Ok(Self {
            #[cfg(windows)]
            inherited_console: WindowsConsoleModes::capture()?,
        })
    }

    #[cfg(windows)]
    fn prepare_ansi_output(&self) -> io::Result<()> {
        self.inherited_console.enable_vt_output()
    }
}

impl ModeBackend for CrosstermModes {
    fn enable(&mut self, mode: TerminalMode) -> io::Result<()> {
        match mode {
            TerminalMode::Raw => enable_raw_mode(),
            TerminalMode::AlternateScreen => {
                #[cfg(windows)]
                self.prepare_ansi_output()?;
                execute!(io::stdout(), EnterAlternateScreen)
            }
            TerminalMode::MouseCapture => {
                execute!(io::stdout(), EnableMouseCapture)
            }
            TerminalMode::PointerShape => PointerShape::Default.write(&mut io::stdout().lock()),
            #[cfg(unix)]
            TerminalMode::FocusChange => {
                execute!(io::stdout(), EnableFocusChange)
            }
            #[cfg(unix)]
            TerminalMode::BracketedPaste => {
                execute!(io::stdout(), EnableBracketedPaste)
            }
            #[cfg(unix)]
            TerminalMode::KeyboardEnhancement => {
                execute!(
                    io::stdout(),
                    crossterm::event::PushKeyboardEnhancementFlags(
                        crossterm::event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    )
                )
            }
        }
    }

    fn disable(&mut self, mode: TerminalMode) -> io::Result<()> {
        match mode {
            TerminalMode::Raw => disable_raw_mode(),
            TerminalMode::AlternateScreen => {
                #[cfg(windows)]
                self.prepare_ansi_output()?;
                execute!(io::stdout(), LeaveAlternateScreen)
            }
            TerminalMode::MouseCapture => {
                execute!(io::stdout(), DisableMouseCapture)
            }
            TerminalMode::PointerShape => reset_pointer(&mut io::stdout().lock()),
            #[cfg(unix)]
            TerminalMode::FocusChange => {
                execute!(io::stdout(), DisableFocusChange)
            }
            #[cfg(unix)]
            TerminalMode::BracketedPaste => {
                execute!(io::stdout(), DisableBracketedPaste)
            }
            #[cfg(unix)]
            TerminalMode::KeyboardEnhancement => {
                execute!(io::stdout(), crossterm::event::PopKeyboardEnhancementFlags)
            }
        }
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        #[cfg(windows)]
        self.prepare_ansi_output()?;
        execute!(io::stdout(), Show)
    }

    fn restore_host_console(&mut self) -> io::Result<()> {
        #[cfg(windows)]
        return self.inherited_console.restore();
        #[cfg(not(windows))]
        Ok(())
    }
}

#[cfg(windows)]
struct WindowsConsoleModes {
    input: crossterm_winapi::ConsoleMode,
    input_mode: u32,
    output: crossterm_winapi::ConsoleMode,
    output_mode: u32,
}

#[cfg(windows)]
impl WindowsConsoleModes {
    fn capture() -> io::Result<Self> {
        use crossterm_winapi::{ConsoleMode, Handle};

        // The wrapper makes the underlying GetStdHandle/GetConsoleMode calls
        // without weakening this crate's process-wide `forbid(unsafe_code)`
        // boundary.
        let input = ConsoleMode::from(Handle::input_handle()?);
        let output = ConsoleMode::from(Handle::output_handle()?);
        Ok(Self {
            input_mode: input.mode()?,
            output_mode: output.mode()?,
            input,
            output,
        })
    }

    fn enable_vt_output(&self) -> io::Result<()> {
        // https://learn.microsoft.com/windows/console/setconsolemode
        const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;

        let current = self.output.mode()?;
        if current & ENABLE_VIRTUAL_TERMINAL_PROCESSING == 0 {
            self.output
                .set_mode(current | ENABLE_VIRTUAL_TERMINAL_PROCESSING)?;
        }
        Ok(())
    }

    fn restore(&self) -> io::Result<()> {
        let mut first_error = None;
        for (console, mode) in [
            (&self.input, self.input_mode),
            (&self.output, self.output_mode),
        ] {
            // ConsoleMode::set_mode is the safe SetConsoleMode boundary.
            if let Err(error) = console.set_mode(mode)
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

/// Owns the temporary terminal modes independently of the renderer.
///
/// Keeping this lease separate lets `TerminalSession` install its quiet panic
/// hook before any temporary mode becomes observable. The session remains the
/// sole owner and restores the ledger after its input worker has joined.
pub(super) struct ModeLease {
    ledger: ModeLedger<CrosstermModes>,
    pointer_shape: Option<PointerShape>,
}

impl ModeLease {
    pub(super) fn prepare() -> io::Result<Self> {
        Ok(Self {
            ledger: ModeLedger::new(CrosstermModes::capture()?),
            pointer_shape: None,
        })
    }

    /// Acquire the temporary modes for initial entry or after suspension.
    pub(super) fn acquire(&mut self) -> io::Result<TerminalCapabilities> {
        #[cfg(windows)]
        self.ledger.restore_host_console_on_exit();
        self.ledger.enable(TerminalMode::Raw)?;
        self.ledger.enable(TerminalMode::AlternateScreen)?;
        self.ledger.enable(TerminalMode::MouseCapture)?;
        self.ledger.enable(TerminalMode::PointerShape)?;
        self.pointer_shape = Some(PointerShape::Default);
        // Native Windows consoles report focus changes without a mode toggle.
        // Restricting this ANSI protocol to Unix also keeps the exact Windows
        // console snapshot as the only focus-related host contract.
        #[cfg(unix)]
        self.ledger.enable(TerminalMode::FocusChange)?;
        // Crossterm 0.28 parses bracketed paste only in its Unix event backend.
        // Native Windows uses console key records, so enabling DECSET 2004 there
        // would advertise an Event::Paste capability that the reader cannot emit.
        #[cfg(unix)]
        self.ledger.enable(TerminalMode::BracketedPaste)?;

        // Probe before the input worker becomes the process's only reader.
        // Crossterm preserves unrelated input while it waits for the protocol
        // reply, so keys typed during startup are replayed to the app.
        let capabilities = detect_capabilities();
        #[cfg(unix)]
        if capabilities.uses_keyboard_enhancement() {
            self.ledger.enable(TerminalMode::KeyboardEnhancement)?;
        }

        // Ratatui owns cursor visibility while drawing. We do not change its
        // color or shape, but always return visibility to the user's terminal.
        self.ledger.restore_cursor_on_exit();
        Ok(capabilities)
    }

    pub(super) fn restore(&mut self) -> io::Result<()> {
        self.pointer_shape = None;
        self.ledger.restore()
    }

    pub(super) fn set_pointer_shape(&mut self, shape: PointerShape) -> io::Result<()> {
        if self.pointer_shape != Some(shape) {
            shape.write(&mut io::stdout().lock())?;
            self.pointer_shape = Some(shape);
        }
        Ok(())
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum Call {
        Enable(TerminalMode),
        Disable(TerminalMode),
        ShowCursor,
        RestoreHostConsole,
    }

    #[derive(Default)]
    struct RecordingBackend {
        calls: Vec<Call>,
        enable_failure: Option<TerminalMode>,
        disable_failures: Vec<TerminalMode>,
        cursor_failure: bool,
        host_restore_failure: bool,
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

        fn restore_host_console(&mut self) -> io::Result<()> {
            self.calls.push(Call::RestoreHostConsole);
            if self.host_restore_failure {
                Err(io::Error::other("could not restore host console"))
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
        ledger.enable(TerminalMode::MouseCapture).unwrap();
        ledger.restore_cursor_on_exit();

        ledger.restore().unwrap();

        assert_eq!(
            ledger.backend.calls,
            [
                Call::Enable(TerminalMode::Raw),
                Call::Enable(TerminalMode::AlternateScreen),
                Call::Enable(TerminalMode::MouseCapture),
                Call::ShowCursor,
                Call::Disable(TerminalMode::MouseCapture),
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
    fn failed_pointer_setup_is_reset_before_leaving_the_alternate_screen() {
        let backend = RecordingBackend {
            enable_failure: Some(TerminalMode::PointerShape),
            ..RecordingBackend::default()
        };
        let mut ledger = ModeLedger::new(backend);
        ledger.enable(TerminalMode::AlternateScreen).unwrap();
        assert!(ledger.enable(TerminalMode::PointerShape).is_err());
        ledger.restore().unwrap();
        assert!(ledger.backend.calls.ends_with(&[
            Call::Disable(TerminalMode::PointerShape),
            Call::Disable(TerminalMode::AlternateScreen),
        ]));
    }

    #[cfg(unix)]
    #[test]
    fn failed_focus_enable_is_still_compensated() {
        let backend = RecordingBackend {
            enable_failure: Some(TerminalMode::FocusChange),
            ..RecordingBackend::default()
        };
        let mut ledger = ModeLedger::new(backend);
        ledger.enable(TerminalMode::Raw).unwrap();
        ledger.enable(TerminalMode::AlternateScreen).unwrap();
        ledger.enable(TerminalMode::MouseCapture).unwrap();

        assert!(ledger.enable(TerminalMode::FocusChange).is_err());
        ledger.restore().unwrap();

        assert_eq!(
            ledger.backend.calls,
            [
                Call::Enable(TerminalMode::Raw),
                Call::Enable(TerminalMode::AlternateScreen),
                Call::Enable(TerminalMode::MouseCapture),
                Call::Enable(TerminalMode::FocusChange),
                Call::Disable(TerminalMode::FocusChange),
                Call::Disable(TerminalMode::MouseCapture),
                Call::Disable(TerminalMode::AlternateScreen),
                Call::Disable(TerminalMode::Raw),
            ]
        );
    }

    #[test]
    fn cleanup_continues_after_errors_and_is_idempotent() {
        let backend = RecordingBackend {
            disable_failures: vec![TerminalMode::MouseCapture],
            cursor_failure: true,
            ..RecordingBackend::default()
        };
        let mut ledger = ModeLedger::new(backend);
        ledger.enable(TerminalMode::Raw).unwrap();
        ledger.enable(TerminalMode::AlternateScreen).unwrap();
        ledger.enable(TerminalMode::MouseCapture).unwrap();
        ledger.restore_cursor_on_exit();

        let error = ledger.restore().unwrap_err();
        assert_eq!(error.to_string(), "could not show cursor");
        let calls_after_first_restore = ledger.backend.calls.clone();

        ledger.backend.cursor_failure = false;
        ledger.backend.disable_failures.clear();
        ledger.restore().unwrap();
        assert!(calls_after_first_restore.ends_with(&[
            Call::ShowCursor,
            Call::Disable(TerminalMode::MouseCapture),
            Call::Disable(TerminalMode::AlternateScreen),
            Call::Disable(TerminalMode::Raw),
        ]));
        assert!(
            ledger
                .backend
                .calls
                .ends_with(&[Call::ShowCursor, Call::Disable(TerminalMode::MouseCapture),])
        );
        let calls_after_retry = ledger.backend.calls.clone();
        ledger.restore().unwrap();
        assert_eq!(ledger.backend.calls, calls_after_retry);
    }

    #[test]
    fn exact_host_restore_remains_a_barrier_until_dependent_cleanup_succeeds() {
        let backend = RecordingBackend {
            disable_failures: vec![TerminalMode::Raw],
            ..RecordingBackend::default()
        };
        let mut ledger = ModeLedger::new(backend);
        ledger.restore_host_console_on_exit();
        ledger.enable(TerminalMode::Raw).unwrap();
        ledger.enable(TerminalMode::AlternateScreen).unwrap();

        assert!(ledger.restore().is_err());
        assert!(ledger.backend.calls.ends_with(&[
            Call::Disable(TerminalMode::AlternateScreen),
            Call::Disable(TerminalMode::Raw),
            Call::RestoreHostConsole,
        ]));

        ledger.backend.disable_failures.clear();
        ledger.restore().unwrap();
        assert!(
            ledger
                .backend
                .calls
                .ends_with(&[Call::Disable(TerminalMode::Raw), Call::RestoreHostConsole])
        );
        let calls_after_retry = ledger.backend.calls.clone();
        ledger.restore().unwrap();
        assert_eq!(ledger.backend.calls, calls_after_retry);
    }
}
