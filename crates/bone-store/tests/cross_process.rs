#![cfg(unix)]

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Child, Command},
    thread,
    time::{Duration, Instant},
};

use bone_store::{BoneStore, LeaseKey, StoreError, StoreRoots};

const HELPER_MODE: &str = "BONE_STORE_TEST_HELPER_MODE";
const DATA_ROOT: &str = "BONE_STORE_TEST_DATA_ROOT";
const READY_FILE: &str = "BONE_STORE_TEST_READY_FILE";
const RELEASE_FILE: &str = "BONE_STORE_TEST_RELEASE_FILE";

const GENERIC_LEASE: &str = "generic-lease";

#[test]
fn child_helper() {
    let Some(mode) = env::var_os(HELPER_MODE) else {
        return;
    };
    match mode.to_string_lossy().as_ref() {
        GENERIC_LEASE => hold_lease(),
        other => panic!("unknown bone-store test helper mode: {other}"),
    }
}

#[test]
fn generic_lease_is_exclusive_across_processes() {
    let temporary = private_tempdir();
    let roots = roots(&temporary);
    let (ready, release) = control_files(&temporary);
    let mut helper = spawn_lease_helper(&roots, &ready, &release);
    wait_for_ready(&ready, &mut helper);

    let store = BoneStore::open_at(roots.clone()).unwrap();
    assert!(matches!(
        store.try_acquire_lease(LeaseKey::new("session:one")),
        Err(StoreError::Busy)
    ));
    release_and_wait(helper);
    store
        .try_acquire_lease(LeaseKey::new("session:one"))
        .unwrap();
}

fn hold_lease() {
    let store = BoneStore::open_at(roots_from_environment()).unwrap();
    let _lease = store
        .try_acquire_lease(LeaseKey::new("session:one"))
        .unwrap();
    signal_ready_and_wait_for_release();
}

fn roots(temporary: &tempfile::TempDir) -> StoreRoots {
    StoreRoots::new(temporary.path().join("data")).unwrap()
}

fn private_tempdir() -> tempfile::TempDir {
    let temporary = tempfile::tempdir().unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700)).unwrap();
    temporary
}

fn control_files(temporary: &tempfile::TempDir) -> (PathBuf, PathBuf) {
    (
        temporary.path().join("lease-ready"),
        temporary.path().join("lease-release"),
    )
}

fn spawn_lease_helper(roots: &StoreRoots, ready: &Path, release: &Path) -> LeaseHelper {
    let child = helper_command(GENERIC_LEASE)
        .env(DATA_ROOT, roots.data_root())
        .env(READY_FILE, ready)
        .env(RELEASE_FILE, release)
        .spawn()
        .unwrap();
    LeaseHelper {
        child,
        release: release.to_owned(),
    }
}

fn helper_command(mode: &str) -> Command {
    let current_test_binary = env::current_exe().unwrap();
    let mut command = Command::new(current_test_binary);
    command
        .arg("--exact")
        .arg("child_helper")
        .arg("--nocapture")
        .env(HELPER_MODE, mode);
    command
}

fn roots_from_environment() -> StoreRoots {
    StoreRoots::new(PathBuf::from(env::var_os(DATA_ROOT).unwrap())).unwrap()
}

fn signal_ready_and_wait_for_release() {
    let ready = PathBuf::from(env::var_os(READY_FILE).unwrap());
    let release = PathBuf::from(env::var_os(RELEASE_FILE).unwrap());
    fs::write(ready, b"ready").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !release.exists() {
        assert!(
            Instant::now() < deadline,
            "parent did not release test lease"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for_ready(ready: &Path, helper: &mut LeaseHelper) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready.exists() {
        if let Some(status) = helper.child.try_wait().unwrap() {
            panic!("lease helper exited before becoming ready: {status}");
        }
        assert!(
            Instant::now() < deadline,
            "lease helper did not become ready"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn release_and_wait(mut helper: LeaseHelper) {
    fs::write(&helper.release, b"release").unwrap();
    let status = helper.child.wait().unwrap();
    assert!(status.success(), "lease helper failed: {status}");
}

struct LeaseHelper {
    child: Child,
    release: PathBuf,
}

impl Drop for LeaseHelper {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_some() {
            return;
        }
        let _ = fs::write(&self.release, b"release");
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            if self.child.try_wait().ok().flatten().is_some() {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
