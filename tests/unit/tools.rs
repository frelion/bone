use super::*;

fn assert_installed_but_uncertain(outcome: &ToolOutcome, dir: &Path, name: &str, expected: &str) {
    assert!(outcome.uncertain);
    assert_eq!(outcome.content["effect"], "unknown");
    assert_eq!(std::fs::read_to_string(dir.join(name)).unwrap(), expected);
    assert_eq!(std::fs::read_dir(dir).unwrap().count(), 1);
}

pub(super) fn assert_rejected(outcome: &ToolOutcome, message: &str) {
    assert!(!outcome.uncertain);
    assert!(outcome.content["error"].as_str().unwrap().contains(message));
}

#[test]
fn shell_timeout_accepts_long_builds_and_rejects_invalid_explicit_values() {
    assert_eq!(shell_timeout_seconds(&json!({})).unwrap(), 60);
    for seconds in [1, 60, 900, 3600] {
        assert_eq!(
            shell_timeout_seconds(&json!({"timeout_seconds":seconds})).unwrap(),
            seconds
        );
    }
    for value in [
        json!(0),
        json!(-1),
        json!(3601),
        json!(1.5),
        json!("900"),
        json!(true),
        Value::Null,
    ] {
        let error = shell_timeout_seconds(&json!({"timeout_seconds":value})).unwrap_err();
        assert!(error.to_string().contains("timeout_seconds"));
    }
}

#[test]
fn shell_timeout_schema_matches_runtime_limits() {
    let tool = definitions(true, false)
        .into_iter()
        .find(|tool| tool.name.as_str() == "shell")
        .unwrap();
    let schema = &tool.parameters["properties"]["timeout_seconds"];
    assert_eq!(schema["type"], "integer");
    assert_eq!(schema["minimum"], 1);
    assert_eq!(schema["maximum"], MAX_SHELL_TIMEOUT_SECONDS);
    assert_eq!(schema["default"], DEFAULT_SHELL_TIMEOUT_SECONDS);
    assert_eq!(
        shell_timeout_seconds(&json!({"timeout_seconds":schema["maximum"]})).unwrap(),
        MAX_SHELL_TIMEOUT_SECONDS
    );
}

#[tokio::test]
async fn replacements_require_matching_content() {
    let dir = tempfile::tempdir().unwrap();
    let create = json!({"path":"x","content":"one","expected_sha256":null});
    let replace = json!({"path":"x","content":"two","expected_sha256":sha256(b"one")});
    for (args, succeeds) in [(&create, true), (&create, false), (&replace, true)] {
        let outcome = execute(dir.path(), "write_file", args, None).await;
        assert_eq!(outcome.content.get("error").is_none(), succeeds);
    }
    assert_eq!(
        std::fs::read_to_string(dir.path().join("x")).unwrap(),
        "two"
    );
}
#[test]
fn directory_sync_failure_preserves_unknown_outcome_after_replacement() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("x"), "old").unwrap();
    let args = json!({"path":"x","content":"new","expected_sha256":sha256(b"old")});
    let out = write_file(dir.path(), &args, |_| {
        Err(std::io::Error::other("injected directory sync failure"))
    });
    assert_installed_but_uncertain(&out, dir.path(), "x", "new");
    let rejected = write_file(dir.path(), &args, |_| Ok(()));
    assert!(!rejected.uncertain);
    assert!(rejected.content.get("error").is_some());
}
#[tokio::test]
async fn sparse_oversized_files_are_rejected_before_loading_contents() {
    let dir = tempfile::tempdir().unwrap();
    let file = std::fs::File::create(dir.path().join("huge")).unwrap();
    file.set_len(512 * 1024 * 1024).unwrap();
    let outcome = execute(dir.path(), "read_file", &json!({"path":"huge"}), None).await;
    assert_rejected(&outcome, "16 MiB");
}
#[tokio::test]
async fn directory_listing_limits_entries_and_reports_truncation() {
    let dir = tempfile::tempdir().unwrap();
    for index in 0..1002 {
        std::fs::File::create(dir.path().join(format!("file-{index}"))).unwrap();
    }
    let outcome = execute(dir.path(), "list_files", &json!({}), None).await;
    assert_eq!(outcome.content["entries"].as_array().unwrap().len(), 1000);
    assert_eq!(outcome.content["truncated"], true);
}
#[test]
fn traversal_and_symlink_escapes_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    assert!(workspace_path(dir.path(), "../escape").is_err());
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("/tmp", dir.path().join("outside")).unwrap();
        assert!(workspace_path(dir.path(), "outside/escaped").is_err());
    }
}
#[tokio::test]
async fn shell_output_is_bounded_and_nonzero_exit_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let command = if cfg!(windows) {
        "<nul set /p =test& exit /b 7"
    } else {
        "printf test; exit 7"
    };
    let out = execute(dir.path(), "shell", &json!({"command":command}), None).await;
    assert_eq!(out.content["stdout"], "test");
    assert_eq!(out.content["exit_code"], 7);
    assert!(!out.uncertain);
}

fn assert_raced_write(before_install: impl FnOnce(&Path) -> Result<()>, expected: &str) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("source.py");
    std::fs::write(&path, "original").unwrap();
    let outcome = write_file_prepared(
        directory.path(),
        &json!({"path":"source.py","content":"agent replacement","expected_sha256":sha256(b"original")}),
        |_| Ok(()),
        before_install,
    );
    assert_rejected(&outcome, "changed during");
    assert_eq!(std::fs::read_to_string(path).unwrap(), expected);
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[test]
fn external_save_during_preparation_is_not_overwritten() {
    assert_raced_write(
        |path| {
            std::fs::write(path, "editor save")?;
            Ok(())
        },
        "editor save",
    );
}

#[cfg(any(unix, windows))]
#[test]
fn external_replacement_with_identical_content_is_detected_by_identity() {
    assert_raced_write(
        |path| {
            let other = path.with_extension("editor-save");
            std::fs::write(&other, "original")?;
            std::fs::rename(other, path)?;
            Ok(())
        },
        "original",
    );
}

#[test]
fn new_file_creation_does_not_replace_a_target_created_during_preparation() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("new.py");
    let outcome = write_file_prepared(
        directory.path(),
        &json!({"path":"new.py","content":"agent content","expected_sha256":null}),
        |_| Ok(()),
        |path| {
            std::fs::write(path, "external new file")?;
            Ok(())
        },
    );
    assert!(!outcome.uncertain);
    assert!(outcome.content["error"].is_string());
    assert_eq!(std::fs::read_to_string(path).unwrap(), "external new file");
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[test]
fn new_file_directory_sync_failure_is_unknown_after_the_atomic_create() {
    let directory = tempfile::tempdir().unwrap();
    let outcome = write_file(
        directory.path(),
        &json!({"path":"new.py","content":"new content","expected_sha256":null}),
        |_| Err(std::io::Error::other("injected directory sync failure")),
    );
    assert_installed_but_uncertain(&outcome, directory.path(), "new.py", "new content");
}

#[test]
fn write_hash_rejects_an_oversized_existing_file_before_loading_it() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("growing.log");
    std::fs::File::create(&path)
        .unwrap()
        .set_len(512 * 1024 * 1024)
        .unwrap();
    let outcome = write_file(
        directory.path(),
        &json!({"path":"growing.log","content":"replacement","expected_sha256":"previous-hash"}),
        |_| Ok(()),
    );
    assert_rejected(&outcome, "16 MiB");
    assert_eq!(std::fs::metadata(path).unwrap().len(), 512 * 1024 * 1024);
}

#[cfg(unix)]
#[test]
#[ignore = "subprocess helper for the killed-parent lease test"]
fn inherited_lease_shell_helper() {
    use fs2::FileExt;
    let directory = std::path::PathBuf::from(std::env::var_os("BONE_TOOL_LEASE_TEST_DIR").unwrap());
    let lease = std::sync::Arc::new(
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(directory.join("lease.lock"))
            .unwrap(),
    );
    lease.try_lock_exclusive().unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(execute(
        &directory,
        "shell",
        &json!({"command":"printf ready > shell.started; sleep 2; printf finished > shell.finished","timeout_seconds":10}),
        Some(lease),
    ));
}

#[cfg(unix)]
#[test]
fn killed_parent_does_not_release_the_foreground_shells_physical_lease() {
    use fs2::FileExt;
    use std::process::{Command, Stdio};
    let directory = tempfile::tempdir().unwrap();
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(directory.path().join("lease.lock"))
        .unwrap();
    let mut parent = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "tools::tests::inherited_lease_shell_helper",
            "--ignored",
            "--nocapture",
        ])
        .env("BONE_TOOL_LEASE_TEST_DIR", directory.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let started = std::time::Instant::now();
    while !directory.path().join("shell.started").exists() {
        if started.elapsed() > Duration::from_secs(5) {
            let _ = parent.kill();
            let _ = parent.wait();
            panic!("shell helper did not start");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    parent.kill().unwrap();
    parent.wait().unwrap();
    // This is the exact kernel lock check used when recovering a write.
    assert_eq!(
        lock.try_lock_exclusive().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    let stopped = std::time::Instant::now();
    loop {
        if lock.try_lock_exclusive().is_ok() {
            break;
        }
        assert!(
            stopped.elapsed() < Duration::from_secs(5),
            "orphan shell did not finish"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        std::fs::read_to_string(directory.path().join("shell.finished")).unwrap(),
        "finished"
    );
    FileExt::unlock(&lock).unwrap();
}

#[tokio::test]
async fn observed_shell_drains_beyond_preview_and_keeps_timeout_uncertainty() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let directory = tempfile::tempdir().unwrap();
    let bytes = Arc::new(AtomicUsize::new(0));
    let received = Arc::clone(&bytes);
    let observer: ToolObserver = Arc::new(move |_, chunk| {
        received.fetch_add(chunk.len(), Ordering::Relaxed);
    });
    let command = if cfg!(windows) {
        "powershell.exe -NoProfile -NonInteractive -Command \"[Console]::Out.Write('0123456789'*4000); [Console]::Error.Write('error'); exit 7\""
    } else {
        "i=0; while [ $i -lt 4000 ]; do printf 0123456789; i=$((i + 1)); done; printf error >&2; exit 7"
    };
    let outcome = execute_with_progress(
        directory.path(),
        "shell",
        &json!({"command":command}),
        None,
        Some(Arc::clone(&observer)),
        None,
    )
    .await;
    assert_eq!(bytes.load(Ordering::Relaxed), 40_005);
    assert_eq!(
        outcome.content["stdout"].as_str().unwrap().len(),
        OUTPUT_LIMIT
    );
    assert_eq!(outcome.content["stderr"], "error");
    assert_eq!(outcome.content["exit_code"], 7);
    assert_eq!(outcome.content["truncated"], true);
    assert!(!outcome.uncertain);
    bytes.store(0, Ordering::Relaxed);
    let command = if cfg!(windows) {
        "powershell.exe -NoProfile -NonInteractive -Command \"[Console]::Out.Write('observed-before-timeout'); [Threading.Thread]::Sleep(5000)\""
    } else {
        "printf observed-before-timeout; sleep 5"
    };
    let timed_out = execute_with_progress(
        directory.path(),
        "shell",
        &json!({"command":command, "timeout_seconds":1}),
        None,
        Some(observer),
        None,
    )
    .await;
    assert!(bytes.load(Ordering::Relaxed) > 0);
    assert!(timed_out.uncertain);
    assert_eq!(timed_out.content["effect"], "unknown");
    assert_eq!(timed_out.content["stdout"], "observed-before-timeout");
    assert_eq!(timed_out.content["interrupted"], true);
    assert!(
        timed_out.content["error"]
            .as_str()
            .unwrap()
            .contains("timed out")
    );
}

#[tokio::test]
async fn stop_signal_collects_partial_output_and_terminates_the_command() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = directory.path().to_owned();
    let observed = std::sync::Arc::new(tokio::sync::Notify::new());
    let notify = std::sync::Arc::clone(&observed);
    let observer: ToolObserver = std::sync::Arc::new(move |_, bytes| {
        if !bytes.is_empty() {
            notify.notify_one();
        }
    });
    let (stop, signal) = tokio::sync::watch::channel(false);
    let command = if cfg!(windows) {
        "powershell.exe -NoProfile -NonInteractive -Command \"[Console]::Out.Write('started'); [Threading.Thread]::Sleep(30000); [IO.File]::WriteAllText('must-not-run','bad')\""
    } else {
        "printf started; sleep 30; printf bad > must-not-run"
    };
    let task = tokio::spawn(async move {
        execute_with_progress(
            &workspace,
            "shell",
            &json!({"command":command}),
            None,
            Some(observer),
            Some(signal),
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(15), observed.notified())
        .await
        .unwrap();
    stop.send(true).unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(15), task)
        .await
        .unwrap()
        .unwrap();
    assert!(outcome.uncertain);
    assert_eq!(outcome.content["interrupted"], true);
    assert_eq!(outcome.content["stdout"], "started");
    assert!(
        outcome.content["error"]
            .as_str()
            .unwrap()
            .contains("stopped")
    );
    assert!(!directory.path().join("must-not-run").exists());
}

#[cfg(windows)]
#[tokio::test]
async fn windows_shell_rejects_unc_before_starting_the_command() {
    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().join("must-not-start");
    let command = format!("echo unexpected>\"{}\"", marker.display());
    for workspace in [
        Path::new(r"\\bone-unc-test\missing-share\workspace"),
        Path::new(r"\\?\UNC\bone-unc-test\missing-share\workspace"),
    ] {
        let outcome = execute(workspace, "shell", &json!({"command":command}), None).await;
        assert_rejected(&outcome, "map the workspace to a drive letter");
        assert!(
            !marker.exists(),
            "UNC rejection must precede command execution"
        );
    }
}

#[cfg(windows)]
#[tokio::test]
async fn windows_shell_runs_relative_commands_in_the_canonical_workspace() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = directory.path().canonicalize().unwrap();
    std::fs::write(workspace.join("cwd-evidence"), "workspace only").unwrap();
    let cmd = execute(
        &workspace,
        "shell",
        &json!({"command":"cd & if exist cwd-evidence (exit /b 0) else (exit /b 42)"}),
        None,
    )
    .await;
    assert_eq!(
        cmd.content["exit_code"],
        0,
        "canonical cmd cwd mismatch in {}: {}",
        workspace.display(),
        cmd.content
    );
    let actual = cmd.content["stdout"].as_str().unwrap().trim();
    assert_eq!(
        Path::new(actual).canonicalize().unwrap(),
        workspace,
        "cmd actual working directory differs from the workspace"
    );
    let powershell = execute(&workspace, "shell", &json!({"command":"powershell.exe -NoProfile -NonInteractive -Command \"[Console]::WriteLine([Environment]::CurrentDirectory); if (![IO.File]::Exists('cwd-evidence')) { exit 42 }; [IO.File]::WriteAllText('powershell-cwd-evidence','workspace only')\""}), None).await;
    assert_eq!(
        powershell.content["exit_code"],
        0,
        "PowerShell canonical cwd mismatch in {}: {}",
        workspace.display(),
        powershell.content
    );
    let actual = powershell.content["stdout"].as_str().unwrap().trim();
    assert_eq!(
        Path::new(actual).canonicalize().unwrap(),
        workspace,
        "PowerShell actual location differs from the workspace"
    );
    assert_eq!(
        std::fs::read_to_string(workspace.join("powershell-cwd-evidence")).unwrap(),
        "workspace only",
        "PowerShell relative write must stay in the workspace"
    );
}

#[cfg(windows)]
#[tokio::test]
async fn windows_timeout_stops_descendants_before_releasing_workspace_ownership() {
    use fs2::FileExt;
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("child.ps1"),
        "[Console]::WriteLine('child native='+[Environment]::CurrentDirectory+';script='+$PSScriptRoot); [IO.File]::WriteAllText('child.started','ready'); while (![IO.File]::Exists('release')) { [Threading.Thread]::Sleep(10) }; [IO.File]::WriteAllText('child.finished','finished')",
    )
    .unwrap();
    std::fs::write(directory.path().join("parent.ps1"), "[Console]::WriteLine('parent native='+[Environment]::CurrentDirectory+';script='+$PSScriptRoot); $start=[Diagnostics.ProcessStartInfo]::new('powershell.exe','-NoProfile -NonInteractive -ExecutionPolicy Bypass -File child.ps1'); $start.UseShellExecute=$false; $child=[Diagnostics.Process]::Start($start); while (![IO.File]::Exists('child.started')) { [Threading.Thread]::Sleep(10) }; [IO.File]::WriteAllText('shell.started','ready'); [Threading.Thread]::Sleep(60000)").unwrap();
    let open_lock = |name| {
        std::sync::Arc::new(
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(directory.path().join(name))
                .unwrap(),
        )
    };
    let stable = open_lock("stable.lock");
    stable.try_lock_exclusive().unwrap();
    let progress = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    let observed = progress.clone();
    let observer: ToolObserver =
        std::sync::Arc::new(move |_, bytes| observed.lock().unwrap().extend_from_slice(bytes));
    let outcome = execute_with_progress(directory.path(), "shell", &json!({"command":"powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File parent.ps1","timeout_seconds":15}), Some(stable.clone()), Some(observer), None).await;
    assert!(
        directory.path().join("shell.started").exists(),
        "Windows parent/descendant did not reach ready in {}: {}; progress: {}",
        directory.path().display(),
        outcome.content,
        String::from_utf8_lossy(&progress.lock().unwrap())
    );
    assert!(outcome.uncertain);
    assert_eq!(outcome.content["effect"], "unknown");
    crate::windows::ensure_writer_stopped(&stable).unwrap();
    std::fs::write(directory.path().join("release"), "").unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(
        !directory.path().join("child.finished").exists(),
        "timed-out descendant continued writing"
    );
}

#[cfg(windows)]
#[test]
#[ignore = "subprocess helper for the Windows killed-parent job test"]
fn windows_owned_shell_helper() {
    use fs2::FileExt;
    let directory = std::path::PathBuf::from(std::env::var_os("BONE_TOOL_LEASE_TEST_DIR").unwrap());
    let open = |name| {
        std::sync::Arc::new(
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(directory.join(name))
                .unwrap(),
        )
    };
    let stable = open("stable.lock");
    stable.try_lock_exclusive().unwrap();
    println!(
        "Windows helper requested workspace: {}",
        directory.display()
    );
    let observer: ToolObserver = std::sync::Arc::new(|_, bytes| {
        use std::io::Write;
        let mut output = std::io::stdout().lock();
        output.write_all(bytes).unwrap();
        output.flush().unwrap();
    });
    let outcome = tokio::runtime::Runtime::new().unwrap().block_on(execute_with_progress(&directory, "shell", &json!({"command":"powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File parent.ps1","timeout_seconds":60}), Some(stable), Some(observer), None));
    println!("Windows owned-shell outcome: {}", outcome.content);
}

#[cfg(windows)]
#[test]
fn windows_killed_parent_stops_the_shell_and_keeps_recovery_blocked_until_it_stops() {
    use fs2::FileExt;
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("parent.ps1"), "[Console]::WriteLine('parent native='+[Environment]::CurrentDirectory+';script='+$PSScriptRoot); [IO.File]::WriteAllText('shell.started','ready'); while (![IO.File]::Exists('release')) { [Threading.Thread]::Sleep(10) }; [IO.File]::WriteAllText('shell.finished','finished')").unwrap();
    let open = |name| {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(directory.path().join(name))
            .unwrap()
    };
    let stable = open("stable.lock");
    let helper_log = std::fs::File::create(directory.path().join("helper.log")).unwrap();
    let mut parent = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "tools::tests::windows_owned_shell_helper",
            "--ignored",
            "--nocapture",
        ])
        .env("BONE_TOOL_LEASE_TEST_DIR", directory.path())
        .stdout(helper_log.try_clone().unwrap())
        .stderr(helper_log)
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while !directory.path().join("shell.started").exists() {
        if std::time::Instant::now() >= deadline {
            let _ = parent.kill();
            let _ = parent.wait();
            panic!(
                "Windows shell helper did not start: {}",
                std::fs::read_to_string(directory.path().join("helper.log")).unwrap()
            );
        }
        if let Some(status) = parent.try_wait().unwrap() {
            panic!(
                "Windows shell helper exited before readiness ({status}): {}",
                std::fs::read_to_string(directory.path().join("helper.log")).unwrap()
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    parent.kill().unwrap();
    parent.wait().unwrap();
    stable.try_lock_exclusive().unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while crate::windows::ensure_writer_stopped(&stable).is_err() {
        assert!(
            std::time::Instant::now() < deadline,
            "Windows crashed owner's shell did not stop"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    std::fs::write(directory.path().join("release"), "").unwrap();
    std::thread::sleep(Duration::from_secs(1));
    assert!(!directory.path().join("shell.finished").exists());
}
