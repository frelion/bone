use std::{fmt, fs::File, path::PathBuf};

/// An application-defined name for one fail-fast OS file lease.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LeaseKey(String);

impl LeaseKey {
    pub fn new(key: impl Into<String>) -> Self {
        Self(key.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A process-lifetime, fail-fast OS lease.
///
/// It represents application ownership and is separate from SQLite's short
/// write transactions. Dropping it releases the lock.
pub struct Lease {
    path: PathBuf,
    _file: File,
}

impl Lease {
    pub(crate) fn new(path: PathBuf, file: File) -> Self {
        Self { path, _file: file }
    }
}

impl fmt::Debug for Lease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Lease")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

pub(crate) fn lease_file_name(key: &LeaseKey) -> String {
    let mut encoded = String::with_capacity(key.as_str().len() * 2 + 5);
    for byte in key.as_str().bytes() {
        use std::fmt::Write;
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded.push_str(".lock");
    encoded
}

#[cfg(test)]
mod cross_process_tests {
    use std::{
        env, fs,
        path::{Path, PathBuf},
        process::{Child, Command},
        thread,
        time::{Duration, Instant},
    };

    use super::super::{BoneStore, LeaseKey, StoreError, StoreRoots};

    const HELPER_MODE: &str = "BONE_APP_STORAGE_TEST_HELPER_MODE";
    const DATA_ROOT: &str = "BONE_APP_STORAGE_TEST_DATA_ROOT";
    const READY_FILE: &str = "BONE_APP_STORAGE_TEST_READY_FILE";
    const RELEASE_FILE: &str = "BONE_APP_STORAGE_TEST_RELEASE_FILE";

    #[test]
    fn child_helper() {
        if env::var_os(HELPER_MODE).is_none() {
            return;
        }
        let store = BoneStore::open_at(roots_from_environment()).unwrap();
        let _lease = store
            .try_acquire_lease(LeaseKey::new("session:one"))
            .unwrap();
        signal_ready_and_wait_for_release();
    }

    #[test]
    fn lease_is_exclusive_across_processes() {
        let temporary = private_tempdir();
        let roots = StoreRoots::new(temporary.path().join("data")).unwrap();
        let ready = temporary.path().join("lease-ready");
        let release = temporary.path().join("lease-release");
        let mut helper = spawn_helper(&roots, &ready, &release);
        wait_for_ready(&ready, &mut helper);

        let store = BoneStore::open_at(roots).unwrap();
        assert!(matches!(
            store.try_acquire_lease(LeaseKey::new("session:one")),
            Err(StoreError::Busy)
        ));
        release_and_wait(helper);
        store
            .try_acquire_lease(LeaseKey::new("session:one"))
            .unwrap();
    }

    fn private_tempdir() -> tempfile::TempDir {
        let temporary = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700)).unwrap();
        }
        temporary
    }

    fn spawn_helper(roots: &StoreRoots, ready: &Path, release: &Path) -> LeaseHelper {
        let child = Command::new(env::current_exe().unwrap())
            .arg("--exact")
            .arg("storage::lease::cross_process_tests::child_helper")
            .arg("--nocapture")
            .env(HELPER_MODE, "lease")
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
}
