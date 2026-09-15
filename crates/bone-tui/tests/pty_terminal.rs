#![cfg(unix)]

use std::{
    fs::File,
    io::{Read, Write},
    os::{fd::FromRawFd, unix::process::CommandExt},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[test]
fn real_binary_restores_every_terminal_mode_after_visible_quit_flow() {
    let temporary = tempfile::tempdir().expect("temporary workspace");
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace).expect("workspace directory");
    let mut process = PtyBone::spawn(
        &temporary.path().join("data"),
        &workspace,
        Duration::from_secs(15),
    );

    process.wait_for_bytes(b"\x1b[?1049h");
    process.enable_keyboard_protocol();
    process.write(&[0x04]);
    let status = process.wait_for_exit();
    assert!(status.success(), "bone exited with {status}");
    process.wait_for_bytes(b"\x1b[?1049l");
    let output = process.finish_capture();
    assert_terminal_protocol_restored(&output);
}

#[test]
fn real_binary_never_changes_the_users_cursor_appearance() {
    let temporary = tempfile::tempdir().expect("temporary workspace");
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let mut process = PtyBone::spawn(
        &temporary.path().join("data"),
        &workspace,
        Duration::from_secs(15),
    );
    process.wait_for_bytes(b"\x1b[?1049h");
    process.enable_keyboard_protocol();
    process.write(b"draft");
    process.write(&[0x04]);
    assert!(process.wait_for_exit().success());
    process.wait_for_bytes(b"\x1b[?1049l");
    let output = process.finish_capture();
    assert_no_decorative_terminal_mutations(&output);
}

#[test]
fn real_binary_keeps_the_caret_visible_with_slash_suggestions() {
    let temporary = tempfile::tempdir().expect("temporary workspace");
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let mut process = PtyBone::spawn(
        &temporary.path().join("data"),
        &workspace,
        Duration::from_secs(15),
    );

    process.wait_for_bytes(b"\x1b[?1049h");
    process.enable_keyboard_protocol();
    process.wait_for_bytes(b"\x1b[?25h");
    process.write(b"/");
    process.wait_for_bytes(b"/new");

    let live = process.output.lock().expect("capture lock").clone();
    let last_show = live
        .windows(b"\x1b[?25h".len())
        .rposition(|window| window == b"\x1b[?25h")
        .expect("focused composer shows the native cursor");
    let last_hide = live
        .windows(b"\x1b[?25l".len())
        .rposition(|window| window == b"\x1b[?25l");
    assert!(
        last_hide.is_none_or(|hide| last_show > hide),
        "slash suggestions must not leave the composer cursor hidden"
    );

    process.write(&[0x04]);
    assert!(process.wait_for_exit().success());
    let output = process.finish_capture();
    assert_terminal_protocol_restored(&output);
}

#[test]
fn real_binary_restores_every_terminal_mode_after_unix_termination_signals() {
    for (name, signal) in [
        ("SIGINT", libc::SIGINT),
        ("SIGHUP", libc::SIGHUP),
        ("SIGTERM", libc::SIGTERM),
        ("SIGQUIT", libc::SIGQUIT),
    ] {
        let temporary = tempfile::tempdir().expect("temporary workspace");
        let workspace = temporary.path().join("workspace");
        std::fs::create_dir(&workspace).expect("workspace directory");
        let mut process = PtyBone::spawn(
            &temporary.path().join("data"),
            &workspace,
            Duration::from_secs(15),
        );

        process.wait_for_bytes(b"\x1b[?1049h");
        process.enable_keyboard_protocol();
        process.signal(signal, name);
        let status = process.wait_for_exit();
        assert!(status.success(), "{name} shutdown exited with {status}");
        process.wait_for_bytes(b"\x1b[?1049l");
        let output = process.finish_capture();
        assert_terminal_protocol_restored(&output);
    }
}

#[test]
fn saturated_terminal_input_cannot_delay_signal_restoration() {
    let temporary = tempfile::tempdir().expect("temporary workspace");
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace).expect("workspace directory");
    let mut process = PtyBone::spawn(
        &temporary.path().join("data"),
        &workspace,
        Duration::from_secs(15),
    );

    process.wait_for_bytes(b"\x1b[?1049h");
    process.enable_keyboard_protocol();
    // Rendering each key is slower than the reader, forcing bounded-channel
    // backpressure while the signal asks the reader to stop and join.
    process.saturate_input(b'x');
    process.signal(libc::SIGTERM, "SIGTERM with saturated input");

    assert!(process.wait_for_exit().success());
    process.wait_for_bytes(b"\x1b[?1049l");
    let output = process.finish_capture();
    assert_terminal_protocol_restored(&output);
}

#[cfg(debug_assertions)]
#[test]
fn detached_task_panic_stops_the_runner_and_restores_the_terminal() {
    let temporary = tempfile::tempdir().expect("temporary workspace");
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace).expect("workspace directory");
    let mut process = PtyBone::spawn_with_background_panic(
        &temporary.path().join("data"),
        &workspace,
        Duration::from_secs(15),
    );

    process.wait_for_bytes(b"\x1b[?1049h");
    process.enable_keyboard_protocol();
    let status = process.wait_for_exit();
    assert!(!status.success(), "a detached-task panic must be fatal");
    process.wait_for_bytes(b"\x1b[?1049l");
    let output = process.finish_capture();
    assert_terminal_protocol_restored(&output);
    let leave_screen = find_sequence(&output, b"\x1b[?1049l");
    let restored_output = &output[leave_screen + b"\x1b[?1049l".len()..];
    assert!(
        !restored_output.contains(&0x1b),
        "no TUI ANSI frame bytes may be written after leaving the alternate screen"
    );
    assert!(
        leave_screen < find_sequence(&output, b"injected detached-task panic"),
        "the delegated panic hook must write only after leaving the TUI screen"
    );
    assert!(
        terminal_visible_text(&output).contains("A background task panicked"),
        "the restored shell should receive a concise fatal error"
    );
}

#[test]
fn real_binary_releases_the_terminal_while_suspended_and_reacquires_it_on_continue() {
    let temporary = tempfile::tempdir().expect("temporary workspace");
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace).expect("workspace directory");
    let mut process = PtyBone::spawn(
        &temporary.path().join("data"),
        &workspace,
        Duration::from_secs(15),
    );

    process.wait_for_bytes(b"\x1b[?1049h");
    process.enable_keyboard_protocol_round(1);
    process.signal(libc::SIGTSTP, "SIGTSTP");
    process.wait_for_sequence_count(b"\x1b[?1049l", 1);
    process.wait_until_stopped();
    process.assert_terminal_attributes_restored();

    process.signal(libc::SIGCONT, "SIGCONT");
    process.wait_for_sequence_count(b"\x1b[?1049h", 2);
    process.enable_keyboard_protocol_round(2);
    process.write(&[0x04]);

    let status = process.wait_for_exit();
    assert!(status.success(), "continued BONE exited with {status}");
    process.wait_for_sequence_count(b"\x1b[?1049l", 2);
    let output = process.finish_capture();
    assert_terminal_protocol_restored(&output);
    for sequence in [
        b"\x1b[?1049h".as_slice(),
        b"\x1b[>1u".as_slice(),
        b"\x1b[<1u".as_slice(),
        b"\x1b[?1004h".as_slice(),
        b"\x1b[?1004l".as_slice(),
        b"\x1b[?1049l".as_slice(),
    ] {
        assert_eq!(
            sequence_count(&output, sequence),
            2,
            "suspend/resume must balance terminal sequence {sequence:?}"
        );
    }
}

#[test]
fn real_binary_reports_a_terminal_that_cannot_distinguish_shift_enter() {
    let temporary = tempfile::tempdir().expect("temporary workspace");
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace).expect("workspace directory");
    let mut process = PtyBone::spawn(
        &temporary.path().join("data"),
        &workspace,
        Duration::from_secs(15),
    );

    process.wait_for_bytes(b"\x1b[?1049h");
    process.disable_keyboard_protocol();
    process.wait_for_bytes(b"unavailable");
    process.write(&[0x04]);
    assert!(process.wait_for_exit().success());
    let output = process.finish_capture();
    assert!(
        !output.windows(5).any(|window| window == b"\x1b[>1u"),
        "compatibility mode must not push an unsupported keyboard protocol"
    );
    assert!(
        !output.windows(5).any(|window| window == b"\x1b[<1u"),
        "compatibility mode must not pop a protocol it did not push"
    );
    assert_no_decorative_terminal_mutations(&output);
}

fn assert_terminal_protocol_restored(output: &[u8]) {
    assert_no_decorative_terminal_mutations(output);
    for sequence in [
        b"\x1b[?1049h".as_slice(),
        b"\x1b[>1u".as_slice(),
        b"\x1b[<1u".as_slice(),
        b"\x1b[?2004h".as_slice(),
        b"\x1b[?2004l".as_slice(),
        b"\x1b[?1004h".as_slice(),
        b"\x1b[?1004l".as_slice(),
        b"\x1b[?1000l".as_slice(),
        b"\x1b[?1049l".as_slice(),
        b"\x1b[?25h".as_slice(),
    ] {
        assert!(
            output
                .windows(sequence.len())
                .any(|window| window == sequence),
            "missing terminal protocol sequence {sequence:?}"
        );
    }

    let push = find_sequence(output, b"\x1b[>1u");
    let pop = find_sequence(output, b"\x1b[<1u");
    let bracketed_paste_off = find_sequence(output, b"\x1b[?2004l");
    let focus_change_off = find_sequence(output, b"\x1b[?1004l");
    let leave_screen = find_sequence(output, b"\x1b[?1049l");
    assert!(
        push < pop,
        "keyboard enhancement must be popped after it is pushed"
    );
    assert!(
        pop < bracketed_paste_off
            && bracketed_paste_off < focus_change_off
            && focus_change_off < leave_screen,
        "temporary terminal modes must be released in reverse acquisition order"
    );
    for (enable, disable) in [
        (b"\x1b[?1049h".as_slice(), b"\x1b[?1049l".as_slice()),
        (b"\x1b[>1u".as_slice(), b"\x1b[<1u".as_slice()),
        (b"\x1b[?2004h".as_slice(), b"\x1b[?2004l".as_slice()),
        (b"\x1b[?1004h".as_slice(), b"\x1b[?1004l".as_slice()),
        (b"\x1b[?1000h".as_slice(), b"\x1b[?1000l".as_slice()),
        (b"\x1b[?1002h".as_slice(), b"\x1b[?1002l".as_slice()),
        (b"\x1b[?1003h".as_slice(), b"\x1b[?1003l".as_slice()),
        (b"\x1b[?1015h".as_slice(), b"\x1b[?1015l".as_slice()),
        (b"\x1b[?1006h".as_slice(), b"\x1b[?1006l".as_slice()),
    ] {
        assert_eq!(
            sequence_count(output, enable),
            sequence_count(output, disable),
            "terminal mode must be balanced: {enable:?} / {disable:?}"
        );
        assert!(
            find_last_sequence(output, enable) < find_last_sequence(output, disable),
            "the final terminal mode operation must disable {enable:?}"
        );
    }
}

fn assert_no_decorative_terminal_mutations(output: &[u8]) {
    for sequence in [
        b"\x1b]".as_slice(),
        b"\x9d".as_slice(),
        b"\x1b[0 q".as_slice(),
        b"\x1b[1 q".as_slice(),
        b"\x1b[2 q".as_slice(),
        b"\x1b[3 q".as_slice(),
        b"\x1b[4 q".as_slice(),
        b"\x1b[5 q".as_slice(),
        b"\x1b[6 q".as_slice(),
    ] {
        assert!(
            !output
                .windows(sequence.len())
                .any(|window| window == sequence),
            "BONE must not emit host-persistent or decorative terminal mutations: {sequence:?}"
        );
    }
    assert!(
        !contains_window_resize(output),
        "BONE must not resize the host terminal window"
    );
}

fn contains_window_resize(output: &[u8]) -> bool {
    for start in 0..output.len().saturating_sub(3) {
        if !output[start..].starts_with(b"\x1b[8;") {
            continue;
        }
        let mut separator = false;
        for &byte in &output[start + 4..] {
            match byte {
                b'0'..=b'9' => {}
                b';' => separator = true,
                b't' => return separator,
                // Any other CSI final byte, such as H for cursor position,
                // proves this was not a window resize.
                0x40..=0x7e => break,
                _ => break,
            }
        }
    }
    false
}

fn find_sequence(output: &[u8], sequence: &[u8]) -> usize {
    output
        .windows(sequence.len())
        .position(|window| window == sequence)
        .expect("terminal protocol sequence")
}

fn find_last_sequence(output: &[u8], sequence: &[u8]) -> usize {
    output
        .windows(sequence.len())
        .rposition(|window| window == sequence)
        .expect("terminal protocol sequence")
}

fn sequence_count(output: &[u8], sequence: &[u8]) -> usize {
    output
        .windows(sequence.len())
        .filter(|window| *window == sequence)
        .count()
}

#[derive(Debug, Eq, PartialEq)]
struct TerminalAttributes {
    input_flags: libc::tcflag_t,
    output_flags: libc::tcflag_t,
    control_flags: libc::tcflag_t,
    local_flags: libc::tcflag_t,
    control_characters: Vec<libc::cc_t>,
    input_speed: libc::speed_t,
    output_speed: libc::speed_t,
}

impl TerminalAttributes {
    fn read(file: &File) -> Self {
        use std::{mem::MaybeUninit, os::fd::AsRawFd};

        let mut attributes = MaybeUninit::<libc::termios>::uninit();
        let result = unsafe { libc::tcgetattr(file.as_raw_fd(), attributes.as_mut_ptr()) };
        assert_eq!(
            result,
            0,
            "read PTY termios: {}",
            std::io::Error::last_os_error()
        );
        let attributes = unsafe { attributes.assume_init() };
        Self {
            input_flags: attributes.c_iflag,
            output_flags: attributes.c_oflag,
            control_flags: attributes.c_cflag,
            local_flags: attributes.c_lflag,
            control_characters: attributes.c_cc.to_vec(),
            input_speed: unsafe { libc::cfgetispeed(&attributes) },
            output_speed: unsafe { libc::cfgetospeed(&attributes) },
        }
    }
}

struct PtyBone {
    master: Option<File>,
    child: KillOnDrop,
    output: Arc<Mutex<Vec<u8>>>,
    stop_reader: Arc<AtomicBool>,
    read_task: Option<JoinHandle<()>>,
    inherited_attributes: TerminalAttributes,
    deadline: Instant,
}

impl PtyBone {
    fn spawn(data: &std::path::Path, workspace: &std::path::Path, timeout: Duration) -> Self {
        Self::spawn_with_test_panic(data, workspace, timeout, None)
    }

    #[cfg(debug_assertions)]
    fn spawn_with_background_panic(
        data: &std::path::Path,
        workspace: &std::path::Path,
        timeout: Duration,
    ) -> Self {
        Self::spawn_with_test_panic(data, workspace, timeout, Some(500))
    }

    fn spawn_with_test_panic(
        data: &std::path::Path,
        workspace: &std::path::Path,
        timeout: Duration,
        panic_after_ms: Option<u64>,
    ) -> Self {
        let (master, slave) = open_pty(120, 40);
        let inherited_attributes = TerminalAttributes::read(&master);
        let stdin = slave.try_clone().expect("clone PTY slave");
        let stdout = slave.try_clone().expect("clone PTY slave");
        let stderr = slave;
        let mut command = Command::new(env!("CARGO_BIN_EXE_bone"));
        command
            .arg("--data-dir")
            .arg(data)
            .arg("--workspace")
            .arg(workspace)
            .stdin(Stdio::from(stdin))
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));
        command.env_remove("BONE_TUI_TEST_BACKGROUND_PANIC_AFTER_MS");
        if let Some(delay) = panic_after_ms {
            command.env("BONE_TUI_TEST_BACKGROUND_PANIC_AFTER_MS", delay.to_string());
        }
        // A controlling terminal exercises the same crossterm path as a real
        // shell instead of three unrelated character streams.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY as _, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = KillOnDrop(command.spawn().expect("start bone in PTY"));

        let output = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&output);
        let mut reader = master.try_clone().expect("clone PTY master");
        set_nonblocking(&reader);
        let stop_reader = Arc::new(AtomicBool::new(false));
        let reader_stop = Arc::clone(&stop_reader);
        let read_task = thread::spawn(move || {
            let mut chunk = [0_u8; 4096];
            while !reader_stop.load(Ordering::Acquire) {
                match reader.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(count) => captured
                        .lock()
                        .expect("capture lock")
                        .extend_from_slice(&chunk[..count]),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            master: Some(master),
            child,
            output,
            stop_reader,
            read_task: Some(read_task),
            inherited_attributes,
            deadline: Instant::now() + timeout,
        }
    }

    fn write(&mut self, bytes: &[u8]) {
        self.master
            .as_mut()
            .expect("PTY master")
            .write_all(bytes)
            .expect("write PTY input");
    }

    fn saturate_input(&mut self, byte: u8) {
        let chunk = [byte; 4_096];
        let mut written = 0;
        loop {
            let result = self.master.as_mut().expect("PTY master").write(&chunk);
            match result {
                Ok(0) => panic!("PTY input closed before its buffer became full"),
                Ok(count) => written += count,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => panic!("write PTY input while saturating it: {error}"),
            }
            self.assert_before_deadline("PTY input did not apply backpressure");
        }
        assert!(
            written > 0,
            "PTY input buffer was full before the test wrote"
        );
    }

    fn enable_keyboard_protocol(&mut self) {
        self.enable_keyboard_protocol_round(1);
    }

    fn enable_keyboard_protocol_round(&mut self, round: usize) {
        self.wait_for_sequence_count(b"\x1b[?u", round);
        self.write(b"\x1b[?1u\x1b[?1;2c");
        self.wait_for_sequence_count(b"\x1b[>1u", round);
    }

    fn disable_keyboard_protocol(&mut self) {
        self.wait_for_bytes(b"\x1b[?u");
        // A primary-device-attributes response arriving without a keyboard
        // flags response is the protocol-defined negative capability answer.
        self.write(b"\x1b[?1;2c");
    }

    fn signal(&self, signal: libc::c_int, name: &str) {
        let result = unsafe { libc::kill(self.child.0.id() as libc::pid_t, signal) };
        assert_eq!(
            result,
            0,
            "send {name}: {}",
            std::io::Error::last_os_error()
        );
    }

    fn wait_until_stopped(&self) {
        loop {
            let mut status = 0;
            let result = unsafe {
                libc::waitpid(
                    self.child.0.id() as libc::pid_t,
                    &mut status,
                    libc::WNOHANG | libc::WUNTRACED,
                )
            };
            assert_ne!(
                result,
                -1,
                "wait for BONE to suspend: {}",
                std::io::Error::last_os_error()
            );
            if result > 0 && libc::WIFSTOPPED(status) {
                return;
            }
            self.assert_before_deadline("BONE did not suspend after restoring the terminal");
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait_for_bytes(&self, needle: &[u8]) {
        self.wait_for_sequence_count(needle, 1);
    }

    fn wait_for_sequence_count(&self, needle: &[u8], expected: usize) {
        while !self
            .output
            .lock()
            .expect("capture lock")
            .windows(needle.len())
            .filter(|window| *window == needle)
            .count()
            .ge(&expected)
        {
            self.assert_before_deadline("expected terminal protocol was not emitted");
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait_for_exit(&mut self) -> ExitStatus {
        loop {
            if let Some(status) = self.child.0.try_wait().expect("poll bone") {
                return status;
            }
            self.assert_before_deadline("bone did not exit before the total deadline");
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn finish_capture(&mut self) -> Vec<u8> {
        self.assert_terminal_attributes_restored();
        self.stop_reader.store(true, Ordering::Release);
        self.master.take();
        if let Some(read_task) = self.read_task.take() {
            read_task.join().expect("PTY reader");
        }
        self.output.lock().expect("capture lock").clone()
    }

    fn assert_terminal_attributes_restored(&self) {
        let current = TerminalAttributes::read(self.master.as_ref().expect("PTY master"));
        assert_eq!(
            current, self.inherited_attributes,
            "BONE must restore the complete inherited termios state"
        );
    }

    fn assert_before_deadline(&self, message: &str) {
        if Instant::now() >= self.deadline {
            let captured = self.output.lock().expect("capture lock").clone();
            panic!(
                "{message}; visible: {}; raw: {}",
                terminal_visible_text(&captured),
                String::from_utf8_lossy(&captured)
            );
        }
    }
}

impl Drop for PtyBone {
    fn drop(&mut self) {
        self.stop_reader.store(true, Ordering::Release);
        if self.child.0.try_wait().ok().flatten().is_none() {
            let _ = self.child.0.kill();
            let _ = self.child.0.wait();
        }
        self.master.take();
        if let Some(read_task) = self.read_task.take() {
            let _ = read_task.join();
        }
    }
}

fn set_nonblocking(file: &File) {
    use std::os::fd::AsRawFd;

    let descriptor = file.as_raw_fd();
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
    assert_ne!(flags, -1, "read PTY flags");
    let result = unsafe { libc::fcntl(descriptor, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    assert_ne!(result, -1, "make PTY reader nonblocking");
}

fn open_pty(width: u16, height: u16) -> (File, File) {
    let mut master = -1;
    let mut slave = -1;
    let mut size = libc::winsize {
        ws_row: height,
        ws_col: width,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let result = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::addr_of_mut!(size),
        )
    };
    assert_eq!(
        result,
        0,
        "openpty failed: {}",
        std::io::Error::last_os_error()
    );
    unsafe { (File::from_raw_fd(master), File::from_raw_fd(slave)) }
}

/// Ratatui may move the cursor between every wide character, so assertions on
/// user-visible text must ignore terminal control sequences rather than search
/// the raw byte stream for contiguous UTF-8.
fn terminal_visible_text(bytes: &[u8]) -> String {
    let mut visible = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != 0x1b {
            visible.push(bytes[index]);
            index += 1;
            continue;
        }

        index += 1;
        match bytes.get(index).copied() {
            Some(b'[') => {
                index += 1;
                while index < bytes.len() {
                    let byte = bytes[index];
                    index += 1;
                    if (0x40..=0x7e).contains(&byte) {
                        break;
                    }
                }
            }
            Some(b']' | b'P' | b'X' | b'^' | b'_') => {
                index += 1;
                while index < bytes.len() {
                    match bytes[index] {
                        0x07 => {
                            index += 1;
                            break;
                        }
                        0x1b if bytes.get(index + 1) == Some(&b'\\') => {
                            index += 2;
                            break;
                        }
                        _ => index += 1,
                    }
                }
            }
            Some(_) => index += 1,
            None => {}
        }
    }
    String::from_utf8_lossy(&visible).into_owned()
}

struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

#[test]
fn visible_text_ignores_cursor_and_string_sequences_between_wide_characters() {
    let bytes = concat!(
        "确\x1b[4;10H认",
        "\x1b]0;secret title\x07",
        "退\x1bPprivate payload\x1b\\出"
    )
    .as_bytes();
    assert_eq!(terminal_visible_text(bytes), "确认退出");
}

#[test]
fn clear_modified_enter_and_ctrl_d_preserve_the_exact_unsent_draft() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().join("workspace");
    let data = temporary.path().join("data");
    std::fs::create_dir(&workspace).unwrap();
    let mut process = PtyBone::spawn(&data, &workspace, Duration::from_secs(15));
    process.wait_for_bytes(b"\x1b[?1049h");
    process.enable_keyboard_protocol();
    process.write(b"discard this\x03first\x1b[13;2usecond\x04");
    assert!(process.wait_for_exit().success());
    process.wait_for_bytes(b"\x1b[?1049l");
    let output = process.finish_capture();
    assert_terminal_protocol_restored(&output);
    assert_eq!(
        output
            .windows(5)
            .filter(|bytes| *bytes == b"\x1b[<1u")
            .count(),
        1
    );
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let app = bone_app::App::open(bone_app::AppOptions::isolated(&data))
            .await
            .unwrap();
        let workspace = app.open_workspace(&workspace).await.unwrap();
        let sessions = app.list_sessions(workspace.id).await.unwrap();
        assert_eq!(sessions.len(), 1);
        let snapshot = app
            .session(sessions[0].id)
            .await
            .unwrap()
            .snapshot()
            .await
            .unwrap();
        assert_eq!(snapshot.draft, "first\nsecond");
        assert!(snapshot.inputs.is_empty(), "Shift+Enter must not submit");
        app.shutdown().await.unwrap();
    });
}
