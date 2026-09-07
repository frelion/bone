#![cfg(unix)]

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Child, Command},
    thread,
    time::{Duration, Instant},
};

use bone_store::{BoneStore, ProviderAuthError, ProviderId, StoreError, StoreRoots};

const HELPER_MODE: &str = "BONE_STORE_TEST_HELPER_MODE";
const DATA_ROOT: &str = "BONE_STORE_TEST_DATA_ROOT";
const CONFIG_ROOT: &str = "BONE_STORE_TEST_CONFIG_ROOT";
const READY_FILE: &str = "BONE_STORE_TEST_READY_FILE";
const RELEASE_FILE: &str = "BONE_STORE_TEST_RELEASE_FILE";
const EXPECTED_DATA_ROOT: &str = "BONE_STORE_TEST_EXPECTED_DATA_ROOT";
const EXPECTED_CONFIG_ROOT: &str = "BONE_STORE_TEST_EXPECTED_CONFIG_ROOT";

const SESSION_LEASE: &str = "session-lease";
const PROVIDER_LEASE: &str = "provider-lease";
const XDG_ROOTS: &str = "xdg-roots";

#[test]
fn child_helper() {
    let Some(mode) = env::var_os(HELPER_MODE) else {
        return;
    };

    match mode.to_string_lossy().as_ref() {
        SESSION_LEASE => hold_session_lease(),
        PROVIDER_LEASE => hold_provider_lease(),
        XDG_ROOTS => assert_default_roots(),
        other => panic!("unknown bone-store test helper mode: {other}"),
    }
}

#[test]
fn session_writer_lease_is_exclusive_across_processes() {
    let temporary = private_tempdir();
    let roots = roots(&temporary);
    let (ready, release) = control_files(&temporary);
    let mut helper = spawn_lease_helper(SESSION_LEASE, &roots, &ready, &release);
    wait_for_ready(&ready, &mut helper);

    let store = BoneStore::open_at(roots.clone()).unwrap();
    assert!(matches!(
        store
            .workspace_state()
            .try_acquire_session_writer_lease("session"),
        Err(StoreError::Busy)
    ));

    release_and_wait(helper);
    let _lease = store
        .workspace_state()
        .try_acquire_session_writer_lease("session")
        .unwrap();
}

#[test]
fn provider_auth_lease_is_exclusive_across_processes() {
    let temporary = private_tempdir();
    let roots = roots(&temporary);
    let (ready, release) = control_files(&temporary);
    let mut helper = spawn_lease_helper(PROVIDER_LEASE, &roots, &ready, &release);
    wait_for_ready(&ready, &mut helper);

    let store = BoneStore::open_at(roots.clone()).unwrap();
    assert!(matches!(
        store
            .provider_auth()
            .acquire(ProviderId::ChatGptSubscription),
        Err(ProviderAuthError::Busy)
    ));

    release_and_wait(helper);
    let _lease = store
        .provider_auth()
        .acquire(ProviderId::ChatGptSubscription)
        .unwrap();
}

#[test]
fn default_roots_prefer_absolute_xdg_directories_in_a_clean_process() {
    let temporary = private_tempdir();
    let xdg_data = temporary.path().join("xdg-data");
    let xdg_config = temporary.path().join("xdg-config");
    assert_default_roots_in_child(
        &temporary,
        Some(&xdg_data),
        Some(&xdg_config),
        temporary.path().join("unused-home"),
        xdg_data.join("bone/store-v1"),
        xdg_config.join("bone/store-v1"),
    );
}

#[test]
fn default_roots_fall_back_to_home_when_xdg_variables_are_missing() {
    let temporary = private_tempdir();
    let home = temporary.path().join("home");
    assert_default_roots_in_child(
        &temporary,
        None,
        None,
        home.clone(),
        home.join(".local/share/bone/store-v1"),
        home.join(".config/bone/store-v1"),
    );
}

#[test]
fn default_roots_ignore_relative_xdg_directories_and_use_home() {
    let temporary = private_tempdir();
    let home = temporary.path().join("home");
    assert_default_roots_in_child(
        &temporary,
        Some(Path::new("relative-data")),
        Some(Path::new("relative-config")),
        home.clone(),
        home.join(".local/share/bone/store-v1"),
        home.join(".config/bone/store-v1"),
    );
}

fn hold_session_lease() {
    let roots = roots_from_environment();
    let store = BoneStore::open_at(roots).unwrap();
    let _lease = store
        .workspace_state()
        .try_acquire_session_writer_lease("session")
        .unwrap();
    signal_ready_and_wait_for_release();
}

fn hold_provider_lease() {
    let roots = roots_from_environment();
    let store = BoneStore::open_at(roots).unwrap();
    let _lease = store
        .provider_auth()
        .acquire(ProviderId::ChatGptSubscription)
        .unwrap();
    signal_ready_and_wait_for_release();
}

fn assert_default_roots() {
    let expected_data = PathBuf::from(env::var_os(EXPECTED_DATA_ROOT).unwrap());
    let expected_config = PathBuf::from(env::var_os(EXPECTED_CONFIG_ROOT).unwrap());
    let roots = StoreRoots::default_for_current_user().unwrap();
    assert_eq!(roots.data_root(), expected_data);
    assert_eq!(roots.config_root(), expected_config);
}

fn assert_default_roots_in_child(
    temporary: &tempfile::TempDir,
    xdg_data: Option<&Path>,
    xdg_config: Option<&Path>,
    home: PathBuf,
    expected_data: PathBuf,
    expected_config: PathBuf,
) {
    let mut command = helper_command(XDG_ROOTS);
    command
        .env("HOME", home)
        .env(EXPECTED_DATA_ROOT, expected_data)
        .env(EXPECTED_CONFIG_ROOT, expected_config)
        // These test roots never need to be written; including a path under
        // the temporary directory merely makes any accidental OS diagnostic
        // output easy to keep contained.
        .current_dir(temporary.path());
    set_optional_environment(&mut command, "XDG_DATA_HOME", xdg_data);
    set_optional_environment(&mut command, "XDG_CONFIG_HOME", xdg_config);
    let status = command.status().unwrap();
    assert!(status.success(), "default-root helper failed: {status}");
}

fn set_optional_environment(command: &mut Command, name: &str, value: Option<&Path>) {
    match value {
        Some(value) => {
            command.env(name, value);
        }
        None => {
            command.env_remove(name);
        }
    }
}

fn roots(temporary: &tempfile::TempDir) -> StoreRoots {
    StoreRoots::new(
        temporary.path().join("data"),
        temporary.path().join("config"),
    )
    .unwrap()
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

fn spawn_lease_helper(mode: &str, roots: &StoreRoots, ready: &Path, release: &Path) -> LeaseHelper {
    let child = helper_command(mode)
        .env(DATA_ROOT, roots.data_root())
        .env(CONFIG_ROOT, roots.config_root())
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
    StoreRoots::new(
        PathBuf::from(env::var_os(DATA_ROOT).unwrap()),
        PathBuf::from(env::var_os(CONFIG_ROOT).unwrap()),
    )
    .unwrap()
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
