use std::{
    cell::Cell,
    io::{self, Write},
    marker::PhantomData,
    panic,
    rc::Rc,
    sync::{
        Arc, Condvar, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle, ThreadId},
    time::Duration,
};

use crossterm::event::{self, Event};
use ratatui::{Terminal, backend::CrosstermBackend};
use tokio::sync::{Notify, mpsc};

use super::{capabilities::TerminalCapabilities, modes::ModeLease};

pub(crate) type TuiTerminal = Terminal<CrosstermBackend<io::Stdout>>;

type PanicHook = dyn for<'a> Fn(&panic::PanicHookInfo<'a>) + Send + Sync + 'static;
type InputMessage = io::Result<Event>;

const INPUT_QUEUE_CAPACITY: usize = 256;
const INPUT_POLL_SLICE: Duration = Duration::from_millis(25);

thread_local! {
    static IN_TERMINAL_INPUT_WORKER: Cell<bool> = const { Cell::new(false) };
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn wait_unpoisoned<'a, T>(condvar: &Condvar, guard: MutexGuard<'a, T>) -> MutexGuard<'a, T> {
    condvar
        .wait(guard)
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct InputControl {
    stopped: AtomicBool,
    capacity_epoch: Mutex<u64>,
    changed: Condvar,
}

impl InputControl {
    fn new() -> Self {
        Self {
            stopped: AtomicBool::new(false),
            capacity_epoch: Mutex::new(0),
            changed: Condvar::new(),
        }
    }

    fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }

    fn capacity_epoch(&self) -> u64 {
        *lock_unpoisoned(&self.capacity_epoch)
    }

    fn capacity_freed(&self) {
        let mut epoch = lock_unpoisoned(&self.capacity_epoch);
        *epoch = epoch.wrapping_add(1);
        self.changed.notify_one();
    }

    fn wait_for_capacity_or_stop(&self, observed: u64) -> bool {
        let mut epoch = lock_unpoisoned(&self.capacity_epoch);
        while *epoch == observed && !self.is_stopped() {
            epoch = wait_unpoisoned(&self.changed, epoch);
        }
        !self.is_stopped()
    }

    fn request_stop(&self) {
        // Participate in the same mutex protocol as the waiter so a stop cannot
        // land between its predicate check and Condvar::wait.
        let mut epoch = lock_unpoisoned(&self.capacity_epoch);
        self.stopped.store(true, Ordering::Release);
        *epoch = epoch.wrapping_add(1);
        self.changed.notify_all();
    }
}

/// The runner-facing half of BONE's single terminal input worker.
pub(crate) struct TerminalEvents {
    receiver: mpsc::Receiver<InputMessage>,
    control: Arc<InputControl>,
}

impl TerminalEvents {
    pub(crate) async fn next(&mut self) -> Option<InputMessage> {
        let event = self.receiver.recv().await;
        if event.is_some() {
            self.control.capacity_freed();
        }
        event
    }

    /// Close and drain a stopped activation's buffered events before resume.
    #[cfg(unix)]
    pub(crate) fn discard(&mut self) {
        self.receiver.close();
        while self.receiver.try_recv().is_ok() {}
    }
}

struct TerminalInputWorker {
    control: Arc<InputControl>,
    thread: Option<JoinHandle<()>>,
}

impl TerminalInputWorker {
    fn spawn() -> io::Result<(Self, TerminalEvents)> {
        let (sender, receiver) = mpsc::channel(INPUT_QUEUE_CAPACITY);
        let control = Arc::new(InputControl::new());
        let worker_control = Arc::clone(&control);
        let worker = thread::Builder::new()
            .name("bone-terminal-input".into())
            .spawn(move || {
                IN_TERMINAL_INPUT_WORKER.with(|role| role.set(true));
                run_input_worker(sender, &worker_control)
            })?;
        Ok((
            Self {
                control: Arc::clone(&control),
                thread: Some(worker),
            },
            TerminalEvents { receiver, control },
        ))
    }

    fn stop_and_join(&mut self) -> io::Result<()> {
        self.control.request_stop();
        let Some(worker) = self.thread.take() else {
            return Ok(());
        };
        worker.join().map_err(|_| {
            io::Error::other("terminal input worker panicked before it could be joined")
        })
    }
}

impl Drop for TerminalInputWorker {
    fn drop(&mut self) {
        let _ = self.stop_and_join();
    }
}

enum Delivery {
    Sent,
    Stopped,
    ReceiverGone,
}

fn deliver_losslessly(
    sender: &mpsc::Sender<InputMessage>,
    control: &InputControl,
    mut event: InputMessage,
) -> Delivery {
    loop {
        if control.is_stopped() {
            return Delivery::Stopped;
        }
        // Snapshot before try_send. A receiver that frees capacity in between
        // advances the epoch, preventing a lost wake if the queue remains full.
        let observed = control.capacity_epoch();
        match sender.try_send(event) {
            Ok(()) => return Delivery::Sent,
            Err(mpsc::error::TrySendError::Closed(_)) => return Delivery::ReceiverGone,
            Err(mpsc::error::TrySendError::Full(returned)) => {
                event = returned;
                if !control.wait_for_capacity_or_stop(observed) {
                    return Delivery::Stopped;
                }
            }
        }
    }
}

fn run_input_worker(sender: mpsc::Sender<InputMessage>, control: &InputControl) {
    loop {
        if control.is_stopped() {
            return;
        }

        let (message, exit_after_delivery) = match event::poll(INPUT_POLL_SLICE) {
            Ok(false) => continue,
            Ok(true) => {
                // `poll(true)` may stage an event in Crossterm's process-global
                // reader. Always consume that selected event in this generation,
                // then let stop discard it instead of leaving that item to resume.
                let Some(result) = read_ready_then_observe_stop(control, event::read) else {
                    return;
                };
                match result {
                    Ok(event) => (Ok(event), false),
                    Err(error) => (Err(error), true),
                }
            }
            Err(error) => (Err(error), true),
        };
        match deliver_losslessly(&sender, control, message) {
            Delivery::Sent if !exit_after_delivery => {}
            Delivery::Sent | Delivery::Stopped | Delivery::ReceiverGone => return,
        }
    }
}

fn read_ready_then_observe_stop<T>(control: &InputControl, read: impl FnOnce() -> T) -> Option<T> {
    let value = read();
    (!control.is_stopped()).then_some(value)
}

fn stop_input_before_restore(
    input: &mut Option<TerminalInputWorker>,
    after_join: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    let joined = match input.as_mut() {
        Some(input) => input.stop_and_join(),
        None => Ok(()),
    };
    input.take();
    // Never skip mode restoration when the worker itself panicked.
    let join_error = joined.err();
    let restore = after_join();
    match (join_error, restore) {
        (Some(join_error), Err(restore_error)) => Err(combine_io_errors(
            "terminal input worker join failed",
            join_error,
            "terminal mode restoration also failed",
            restore_error,
        )),
        (Some(error), Ok(())) => Err(error),
        (None, result) => result,
    }
}

fn combine_io_errors(
    primary_context: &str,
    primary: io::Error,
    cleanup_context: &str,
    cleanup: io::Error,
) -> io::Error {
    let kind = primary.kind();
    io::Error::new(
        kind,
        format!("{primary_context}: {primary}; {cleanup_context}: {cleanup}"),
    )
}

struct RestoreGate {
    restored: Mutex<bool>,
    changed: Condvar,
}

impl RestoreGate {
    fn closed() -> Self {
        Self {
            restored: Mutex::new(false),
            changed: Condvar::new(),
        }
    }

    fn open(&self) {
        let mut restored = lock_unpoisoned(&self.restored);
        *restored = true;
        self.changed.notify_all();
    }

    fn wait(&self) {
        let mut restored = lock_unpoisoned(&self.restored);
        while !*restored {
            restored = wait_unpoisoned(&self.changed, restored);
        }
    }
}

struct PanicHookState {
    active: AtomicBool,
    current_gate: Mutex<Option<Arc<RestoreGate>>>,
    local_panicked: AtomicBool,
    emitted: AtomicBool,
}

impl PanicHookState {
    fn new() -> Self {
        Self {
            active: AtomicBool::new(false),
            current_gate: Mutex::new(None),
            local_panicked: AtomicBool::new(false),
            emitted: AtomicBool::new(false),
        }
    }

    fn activate(&self) {
        debug_assert!(!self.active.load(Ordering::Acquire));
        let mut current = lock_unpoisoned(&self.current_gate);
        debug_assert!(current.is_none());
        *current = Some(Arc::new(RestoreGate::closed()));
        self.active.store(true, Ordering::Release);
    }

    fn deactivate_after_restore(&self) {
        let gate = lock_unpoisoned(&self.current_gate).take();
        if let Some(gate) = gate {
            gate.open();
        }
        self.active.store(false, Ordering::Release);
    }

    fn record_local_panic(&self) {
        self.local_panicked.store(true, Ordering::Release);
    }

    fn emit_after_restore(&self) {
        if !self.local_panicked.load(Ordering::Acquire) || self.emitted.swap(true, Ordering::AcqRel)
        {
            return;
        }
        // Avoid eprintln!: its internal write failure panics. This path may run
        // while the runner itself is already unwinding.
        let mut stderr = io::stderr().lock();
        let _ = stderr.write_all(b"BONE terminal restored after runner/input-worker panic\n");
    }
}

/// A process-wide panic is fatal to the active terminal session.
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

/// Owns the renderer, input thread, panic boundary, and temporary modes.
pub(crate) struct TerminalSession {
    terminal: Option<TuiTerminal>,
    modes: ModeLease,
    input: Option<TerminalInputWorker>,
    previous_hook: Option<Arc<PanicHook>>,
    hook_state: Arc<PanicHookState>,
    panic_signal: PanicSignal,
    capabilities: Option<TerminalCapabilities>,
    owner_thread: ThreadId,
    _not_send: PhantomData<Rc<()>>,
}

impl TerminalSession {
    pub(crate) fn enter() -> io::Result<(Self, TerminalEvents)> {
        // Capturing inherited state does not mutate the terminal, so failure
        // here needs no panic or input cleanup boundary.
        let modes = ModeLease::prepare()?;
        let panic_signal = PanicSignal::new();
        let mut session = Self {
            terminal: None,
            modes,
            input: None,
            previous_hook: None,
            hook_state: Arc::new(PanicHookState::new()),
            panic_signal,
            capabilities: None,
            owner_thread: thread::current().id(),
            _not_send: PhantomData,
        };
        // Keep one hook installed for the whole session, including while the
        // process is suspended. Each activation supplies it a fresh gate.
        session.install_panic_hook();
        match session.activate() {
            Ok(events) => Ok((session, events)),
            Err(activation_error) => match session.restore() {
                Ok(()) => Err(activation_error),
                Err(cleanup_error) => Err(combine_io_errors(
                    "terminal activation failed",
                    activation_error,
                    "terminal cleanup also failed",
                    cleanup_error,
                )),
            },
        }
    }

    pub(crate) fn terminal(&mut self) -> &mut TuiTerminal {
        self.terminal
            .as_mut()
            .expect("terminal session remains active while borrowed")
    }

    pub(crate) fn capabilities(&self) -> &TerminalCapabilities {
        self.capabilities
            .as_ref()
            .expect("terminal capabilities exist while the session is active")
    }

    pub(crate) fn panic_signal(&self) -> PanicSignal {
        self.panic_signal.clone()
    }

    /// Stops and joins the reader before returning the terminal to the shell.
    pub(crate) fn restore(&mut self) -> io::Result<()> {
        self.deactivate(true)
    }

    /// Returns the terminal to the shell while retaining the session hook.
    #[cfg(unix)]
    pub(crate) fn suspend(&mut self) -> io::Result<()> {
        self.deactivate(false)
    }

    /// Re-enter after continuation and return the replacement reader.
    #[cfg(unix)]
    pub(crate) fn resume(&mut self) -> io::Result<TerminalEvents> {
        match self.activate() {
            Ok(events) => Ok(events),
            Err(activation_error) => match self.restore() {
                Ok(()) => Err(activation_error),
                Err(cleanup_error) => Err(combine_io_errors(
                    "terminal resume failed",
                    activation_error,
                    "terminal cleanup also failed",
                    cleanup_error,
                )),
            },
        }
    }

    fn activate(&mut self) -> io::Result<TerminalEvents> {
        debug_assert!(self.terminal.is_none());
        debug_assert!(self.input.is_none());
        debug_assert!(self.previous_hook.is_some());
        if self.panic_signal.is_tripped() {
            return Err(io::Error::other(
                "a background task panicked while the terminal was entering",
            ));
        }

        // A distinct closed gate per activation prevents an old waiter from
        // being confused with a later resume generation.
        self.hook_state.activate();
        if self.panic_signal.is_tripped() {
            return Err(io::Error::other(
                "a background task panicked while terminal entry was armed",
            ));
        }

        let capabilities = self.modes.acquire()?;
        let terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
        self.terminal = Some(terminal);
        let (input, events) = TerminalInputWorker::spawn()?;
        self.input = Some(input);
        self.capabilities = Some(capabilities);
        if self.panic_signal.is_tripped() {
            return Err(io::Error::other(
                "a background task panicked while terminal input was starting",
            ));
        }
        Ok(events)
    }

    fn deactivate(&mut self, final_restore: bool) -> io::Result<()> {
        let input = &mut self.input;
        let terminal = &mut self.terminal;
        let modes = &mut self.modes;
        let result = stop_input_before_restore(input, || {
            terminal.take();
            modes.restore()
        });
        self.capabilities = None;

        // A background panic hook can hold Rust's global hook read lock while
        // waiting. Open before set_hook so the waiter can finish and release
        // that lock; the terminal is already fully restored at this point.
        self.hook_state.deactivate_after_restore();
        if final_restore && !thread::panicking() {
            self.restore_panic_hook();
        }
        self.hook_state.emit_after_restore();
        result
    }

    fn install_panic_hook(&mut self) {
        debug_assert!(self.previous_hook.is_none());
        let previous_hook: Arc<PanicHook> = Arc::from(panic::take_hook());
        let delegated_hook = Arc::clone(&previous_hook);
        let owner_thread = self.owner_thread;
        let panic_signal = self.panic_signal.clone();
        let hook_state = Arc::clone(&self.hook_state);
        panic::set_hook(Box::new(move |info| {
            panic_signal.trip();
            if !hook_state.active.load(Ordering::Acquire) {
                delegated_hook(info);
                return;
            }

            let is_local = thread::current().id() == owner_thread
                || IN_TERMINAL_INPUT_WORKER
                    .try_with(Cell::get)
                    .unwrap_or(false);
            if is_local {
                hook_state.record_local_panic();
                return;
            }

            // This mutex protects only the panic generation pointer; it is
            // independent of the input, renderer, and terminal mode state.
            let gate = lock_unpoisoned(&hook_state.current_gate).clone();
            if let Some(gate) = gate {
                gate.wait();
            }
            delegated_hook(info);
        }));
        self.previous_hook = Some(previous_hook);
    }

    fn restore_panic_hook(&mut self) {
        if let Some(previous) = self.previous_hook.take() {
            panic::set_hook(Box::new(move |info| previous(info)));
        }
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc as std_mpsc;

    #[test]
    fn input_worker_join_completes_before_mode_restoration() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let worker_calls = Arc::clone(&calls);
        let control = Arc::new(InputControl::new());
        let worker_control = Arc::clone(&control);
        let thread = thread::spawn(move || {
            let mut epoch = lock_unpoisoned(&worker_control.capacity_epoch);
            while !worker_control.is_stopped() {
                epoch = wait_unpoisoned(&worker_control.changed, epoch);
            }
            drop(epoch);
            lock_unpoisoned(&worker_calls).push("reader joined");
        });
        let mut input = Some(TerminalInputWorker {
            control,
            thread: Some(thread),
        });

        stop_input_before_restore(&mut input, || {
            lock_unpoisoned(&calls).push("modes restored");
            Ok(())
        })
        .unwrap();

        assert_eq!(
            *lock_unpoisoned(&calls),
            ["reader joined", "modes restored"]
        );
    }

    #[test]
    fn join_and_restore_errors_are_both_reported() {
        let control = Arc::new(InputControl::new());
        let worker_control = Arc::clone(&control);
        let thread = thread::spawn(move || {
            while !worker_control.is_stopped() {
                thread::yield_now();
            }
            panic!("injected input worker failure");
        });
        let mut input = Some(TerminalInputWorker {
            control,
            thread: Some(thread),
        });

        let error = stop_input_before_restore(&mut input, || {
            Err(io::Error::other("injected mode restore failure"))
        })
        .unwrap_err();
        let message = error.to_string();

        assert!(message.contains("input worker panicked"));
        assert!(message.contains("injected mode restore failure"));
    }

    #[test]
    fn background_panic_gate_does_not_pass_before_restore_ack() {
        let gate = Arc::new(RestoreGate::closed());
        let waiter = Arc::clone(&gate);
        let (ready_tx, ready_rx) = std_mpsc::channel();
        let (passed_tx, passed_rx) = std_mpsc::channel();
        let thread = thread::spawn(move || {
            ready_tx.send(()).unwrap();
            waiter.wait();
            passed_tx.send(()).unwrap();
        });

        ready_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(passed_rx.recv_timeout(Duration::from_millis(30)).is_err());
        gate.open();
        passed_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        thread.join().unwrap();
    }

    #[test]
    fn each_terminal_activation_uses_a_fresh_closed_gate() {
        let state = PanicHookState::new();
        state.activate();
        let first = lock_unpoisoned(&state.current_gate)
            .clone()
            .expect("first activation gate");
        assert!(!*lock_unpoisoned(&first.restored));

        state.deactivate_after_restore();
        assert!(*lock_unpoisoned(&first.restored));
        state.activate();
        let second = lock_unpoisoned(&state.current_gate)
            .clone()
            .expect("second activation gate");

        assert!(!Arc::ptr_eq(&first, &second));
        assert!(!*lock_unpoisoned(&second.restored));
        state.deactivate_after_restore();
    }

    #[test]
    fn full_input_queue_wait_is_lossless_and_stop_aware() {
        let (sender, mut receiver) = mpsc::channel(1);
        let control = Arc::new(InputControl::new());
        sender.try_send(Ok(Event::FocusGained)).unwrap();
        let worker_control = Arc::clone(&control);
        let worker = thread::spawn(move || {
            deliver_losslessly(&sender, &worker_control, Ok(Event::FocusLost))
        });

        thread::sleep(Duration::from_millis(20));
        let first = receiver.try_recv().unwrap().unwrap();
        control.capacity_freed();
        assert!(matches!(first, Event::FocusGained));
        assert!(matches!(worker.join().unwrap(), Delivery::Sent));
        assert!(matches!(
            receiver.try_recv().unwrap().unwrap(),
            Event::FocusLost
        ));

        let (sender, _receiver) = mpsc::channel(1);
        sender.try_send(Ok(Event::FocusGained)).unwrap();
        let stopped = Arc::new(InputControl::new());
        let worker_stop = Arc::clone(&stopped);
        let worker =
            thread::spawn(move || deliver_losslessly(&sender, &worker_stop, Ok(Event::FocusLost)));
        thread::sleep(Duration::from_millis(20));
        stopped.request_stop();
        assert!(matches!(worker.join().unwrap(), Delivery::Stopped));
    }

    #[test]
    fn a_ready_event_is_drained_before_stop_is_observed() {
        let control = InputControl::new();
        control.request_stop();
        let read_called = Cell::new(false);

        let value = read_ready_then_observe_stop(&control, || {
            read_called.set(true);
            7
        });

        assert!(read_called.get());
        assert_eq!(value, None);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn discard_closes_and_drains_a_suspended_event_generation() {
        let (sender, receiver) = mpsc::channel(2);
        let control = Arc::new(InputControl::new());
        sender.try_send(Ok(Event::FocusGained)).unwrap();
        sender.try_send(Ok(Event::FocusLost)).unwrap();
        let mut events = TerminalEvents { receiver, control };

        events.discard();

        assert!(events.next().await.is_none());
        assert!(matches!(
            sender.try_send(Ok(Event::FocusGained)),
            Err(mpsc::error::TrySendError::Closed(_))
        ));
    }

    #[tokio::test]
    async fn panic_signal_remembers_a_trip_that_precedes_the_waiter() {
        let signal = PanicSignal::new();
        signal.trip();

        tokio::time::timeout(Duration::from_millis(50), signal.notified())
            .await
            .expect("a prior panic must not be lost");
        assert!(signal.is_tripped());
    }
}
