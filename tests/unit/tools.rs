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
    let out = execute(
        dir.path(),
        "shell",
        &json!({"command":"printf test; exit 7"}),
        None,
    )
    .await;
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

#[cfg(unix)]
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
    let legacy = std::sync::Arc::new(
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(directory.join("legacy.lock"))
            .unwrap(),
    );
    legacy.try_lock_exclusive().unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(execute(
        &directory,
        "shell",
        &json!({"command":"printf ready > shell.started; sleep 2; printf finished > shell.finished","timeout_seconds":10}),
        Some([lease, legacy]),
    ));
}

#[cfg(unix)]
#[test]
fn killed_parent_does_not_release_the_frontground_shells_physical_lease() {
    use fs2::FileExt;
    use std::process::{Command, Stdio};
    let directory = tempfile::tempdir().unwrap();
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(directory.path().join("lease.lock"))
        .unwrap();
    let legacy = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(directory.path().join("legacy.lock"))
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
    assert_eq!(
        legacy.try_lock_exclusive().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    let stopped = std::time::Instant::now();
    loop {
        if lock.try_lock_exclusive().is_ok() && legacy.try_lock_exclusive().is_ok() {
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
    FileExt::unlock(&legacy).unwrap();
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
    let outcome = execute_with_progress(directory.path(), "shell", &json!({"command":"i=0; while [ $i -lt 4000 ]; do printf 0123456789; i=$((i + 1)); done; printf error >&2; exit 7"}), None, Some(Arc::clone(&observer))).await;
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
    let timed_out = execute_with_progress(
        directory.path(),
        "shell",
        &json!({"command":"printf observed-before-timeout; sleep 5", "timeout_seconds":1}),
        None,
        Some(observer),
    )
    .await;
    assert!(bytes.load(Ordering::Relaxed) > 0);
    assert!(timed_out.uncertain);
    assert_eq!(timed_out.content["effect"], "unknown");
    assert!(
        timed_out.content["error"]
            .as_str()
            .unwrap()
            .contains("timed out")
    );
}
