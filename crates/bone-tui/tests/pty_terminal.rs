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
    process.write(&[0x11]);
    process.wait_for_text("确认退出");
    process.write(b"\r");
    let status = process.wait_for_exit();
    assert!(status.success(), "bone exited with {status}");
    process.wait_for_bytes(b"\x1b[?1049l");
    let output = process.finish_capture();
    assert_terminal_protocol_restored(&output);
}

#[test]
fn real_binary_restores_every_terminal_mode_after_unix_termination_signals() {
    for (name, signal) in [
        ("SIGINT", libc::SIGINT),
        ("SIGHUP", libc::SIGHUP),
        ("SIGTERM", libc::SIGTERM),
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
        process.signal(signal, name);
        let status = process.wait_for_exit();
        assert!(status.success(), "{name} shutdown exited with {status}");
        process.wait_for_bytes(b"\x1b[?1049l");
        let output = process.finish_capture();
        assert_terminal_protocol_restored(&output);
    }
}

fn assert_terminal_protocol_restored(output: &[u8]) {
    for sequence in [
        b"\x1b[?1049h".as_slice(),
        b"\x1b[?2004h".as_slice(),
        b"\x1b[?2004l".as_slice(),
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
}

struct PtyBone {
    master: Option<File>,
    child: KillOnDrop,
    output: Arc<Mutex<Vec<u8>>>,
    stop_reader: Arc<AtomicBool>,
    read_task: Option<JoinHandle<()>>,
    deadline: Instant,
}

impl PtyBone {
    fn spawn(data: &std::path::Path, workspace: &std::path::Path, timeout: Duration) -> Self {
        let (master, slave) = open_pty(120, 40);
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
        // A controlling terminal exercises the same crossterm path as a real
        // shell instead of three unrelated character streams.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY, 0) == -1 {
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

    fn signal(&self, signal: libc::c_int, name: &str) {
        let result = unsafe { libc::kill(self.child.0.id() as libc::pid_t, signal) };
        assert_eq!(
            result,
            0,
            "send {name}: {}",
            std::io::Error::last_os_error()
        );
    }

    fn wait_for_bytes(&self, needle: &[u8]) {
        while !self
            .output
            .lock()
            .expect("capture lock")
            .windows(needle.len())
            .any(|window| window == needle)
        {
            self.assert_before_deadline("expected terminal protocol was not emitted");
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait_for_text(&self, needle: &str) {
        loop {
            let captured = self.output.lock().expect("capture lock").clone();
            let visible = terminal_visible_text(&captured);
            if visible.contains(needle) {
                return;
            }
            if Instant::now() >= self.deadline {
                panic!(
                    "PTY visible output never contained {needle:?}; visible: {visible}; raw: {}",
                    String::from_utf8_lossy(&captured)
                );
            }
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
        self.stop_reader.store(true, Ordering::Release);
        self.master.take();
        if let Some(read_task) = self.read_task.take() {
            read_task.join().expect("PTY reader");
        }
        self.output.lock().expect("capture lock").clone()
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
    let size = libc::winsize {
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
            std::ptr::null(),
            &size,
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
