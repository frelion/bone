#![cfg(unix)]

use std::{
    ffi::OsString,
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, mpsc},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use rig_core::{
    completion::{CompletionError, CompletionModel, CompletionRequest, CompletionResponse},
    streaming::StreamingCompletionResponse,
};

use super::credential_store::CredentialLease;
use super::credential_store::{auth_file_path_from, disconnect_at, effective_uid, prepare};
use super::{DeviceCodePrompt, Error, LeasedModel, connect, openai_responses};

#[derive(Clone)]
struct FakeModel;

impl CompletionModel for FakeModel {
    async fn completion(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, CompletionError> {
        unreachable!("lifetime test does not dispatch requests")
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<StreamingCompletionResponse, CompletionError> {
        unreachable!("lifetime test does not dispatch requests")
    }
}

fn temporary_directory(case: &str) -> PathBuf {
    use std::os::unix::fs::DirBuilderExt;

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock should follow the Unix epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "bone-model-chatgpt-{case}-{}-{nonce}",
        std::process::id()
    ));
    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700);
    builder
        .create(&root)
        .expect("test directory should be created");
    root
}

fn temporary_auth_file(case: &str) -> PathBuf {
    temporary_directory(case)
        .join("bone")
        .join("chatgpt-subscription")
        .join("auth.json")
}

fn temporary_root(path: &Path) -> &Path {
    path.parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .expect("test auth path should have a config root")
}

#[test]
fn credential_store_is_private_and_single_owner() {
    let path = temporary_auth_file("private");
    let parent = path.parent().expect("test path should have a parent");
    let first = prepare(&path).expect("first owner should acquire the store");
    assert_eq!(fs::read_to_string(&path).unwrap(), "{}");
    assert_eq!(prepare(&path).unwrap_err(), Error::CredentialStoreBusy);

    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    assert_eq!(fs::metadata(parent).unwrap().uid(), effective_uid());
    assert_eq!(
        fs::metadata(parent).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(parent.join("auth.lock"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );

    drop(first);
    drop(prepare(&path).expect("released store should be reusable"));
    fs::remove_dir_all(temporary_root(&path)).unwrap();
}

#[test]
fn clean_config_roots_are_created_and_invalid_xdg_falls_back_to_home() {
    let parent = temporary_directory("config-root");
    let xdg = parent.join("xdg");
    let xdg_path = auth_file_path_from(Some(xdg.clone().into()), None, true).unwrap();
    assert_eq!(temporary_root(&xdg_path), xdg);
    assert!(xdg.is_dir());

    let home = parent.join("home");
    fs::create_dir(&home).unwrap();
    for invalid_xdg in [OsString::new(), OsString::from("relative/config")] {
        let path = auth_file_path_from(Some(invalid_xdg), Some(home.clone().into()), true)
            .expect("invalid XDG values should fall back to HOME");
        assert_eq!(temporary_root(&path), home.join(".config"));
    }

    assert_eq!(
        auth_file_path_from(None, None, true),
        Err(Error::CredentialStoreUnavailable)
    );
    fs::remove_dir_all(parent).unwrap();
}

#[test]
fn nested_config_roots_are_created_but_writable_roots_are_rejected() {
    use std::os::unix::fs::PermissionsExt;

    let parent = temporary_directory("nested-config-root");
    let nested = parent.join("one").join("two").join("xdg");
    let path = auth_file_path_from(Some(nested.clone().into()), None, true)
        .expect("owned nested XDG roots should be created privately");
    assert_eq!(temporary_root(&path), nested);
    for directory in [parent.join("one"), parent.join("one/two"), nested.clone()] {
        assert_eq!(
            fs::metadata(directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    let writable = parent.join("writable");
    fs::create_dir(&writable).unwrap();
    fs::set_permissions(&writable, fs::Permissions::from_mode(0o777)).unwrap();
    assert_eq!(
        auth_file_path_from(Some(writable.into()), None, true),
        Err(Error::CredentialStoreUnavailable)
    );

    let writable_parent = parent.join("writable-parent");
    let otherwise_private = writable_parent.join("private-xdg");
    fs::create_dir(&writable_parent).unwrap();
    fs::create_dir(&otherwise_private).unwrap();
    fs::set_permissions(&otherwise_private, fs::Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(&writable_parent, fs::Permissions::from_mode(0o777)).unwrap();
    assert_eq!(
        auth_file_path_from(Some(otherwise_private.into()), None, true),
        Err(Error::CredentialStoreUnavailable)
    );
    fs::remove_dir_all(parent).unwrap();
}

#[test]
fn credential_store_rejects_preexisting_readable_credentials() {
    use std::os::unix::fs::PermissionsExt;

    let path = temporary_auth_file("readable-auth");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "{}").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

    assert_eq!(
        prepare(&path).unwrap_err(),
        Error::CredentialStoreUnavailable
    );
    fs::remove_dir_all(temporary_root(&path)).unwrap();
}

#[test]
#[ignore = "subprocess-only credential lock holder"]
fn credential_lock_child() {
    let (Some(path), Some(cookie)) = (
        std::env::var_os("BONE_TEST_CHATGPT_LOCK_PATH"),
        std::env::var_os("BONE_TEST_CHATGPT_LOCK_COOKIE"),
    ) else {
        return;
    };
    if cookie.is_empty() {
        return;
    }

    let _lease = prepare(Path::new(&path)).expect("child should acquire credential lease");
    println!("BONE_TEST_LOCKED:{}", cookie.to_string_lossy());
    std::io::stdout().flush().unwrap();
    std::thread::sleep(Duration::from_secs(60));
}

struct ChildGuard(Option<Child>);

impl ChildGuard {
    fn child_mut(&mut self) -> &mut Child {
        self.0.as_mut().expect("child should still be live")
    }

    fn terminate(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        self.terminate();
    }
}

#[test]
fn credential_store_lock_is_exclusive_across_processes_and_released_on_exit() {
    let path = temporary_auth_file("subprocess");
    let cookie = format!(
        "{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "service::chatgpt_subscription::tests::credential_lock_child",
            "--ignored",
            "--nocapture",
        ])
        .env("BONE_TEST_CHATGPT_LOCK_PATH", &path)
        .env("BONE_TEST_CHATGPT_LOCK_COOKIE", &cookie)
        .stdout(Stdio::piped())
        .spawn()
        .expect("lock-holder child should start");
    let mut child = ChildGuard(Some(child));
    let stdout = child
        .child_mut()
        .stdout
        .take()
        .expect("child stdout should be piped");
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if sender.send(line.unwrap_or_default()).is_err() {
                break;
            }
        }
    });

    let expected = format!("BONE_TEST_LOCKED:{cookie}");
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut locked = false;
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        match receiver.recv_timeout(remaining) {
            Ok(line) if line.contains(&expected) => {
                locked = true;
                break;
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    assert!(locked, "child should report an acquired OS lock");
    assert_eq!(prepare(&path).unwrap_err(), Error::CredentialStoreBusy);

    child.terminate();
    drop(prepare(&path).expect("OS should release a dead process's lock"));
    fs::remove_dir_all(temporary_root(&path)).unwrap();
}

#[test]
fn endpoint_and_selected_model_hold_the_store_lease() {
    let path = temporary_auth_file("lifetime");
    let lease = Arc::new(prepare(&path).unwrap());
    let endpoint = openai_responses::from_completion_model_factory("chatgpt-test", {
        let lease = Arc::clone(&lease);
        move |_model_id| LeasedModel {
            inner: FakeModel,
            _lease: Arc::clone(&lease),
        }
    })
    .unwrap();
    drop(lease);

    assert_eq!(disconnect_at(&path), Err(Error::CredentialStoreBusy));
    let model = endpoint.model("gpt-test").unwrap();
    drop(endpoint);
    assert_eq!(disconnect_at(&path), Err(Error::CredentialStoreBusy));
    drop(model);
    assert_eq!(disconnect_at(&path), Ok(()));
    fs::remove_dir_all(temporary_root(&path)).unwrap();
}

#[test]
fn disconnect_without_a_cache_creates_no_app_artifacts() {
    let root = temporary_directory("never-connected");
    let path = root
        .join("missing-config")
        .join("bone")
        .join("chatgpt-subscription")
        .join("auth.json");
    assert_eq!(disconnect_at(&path), Ok(()));
    assert!(!root.join("missing-config").exists());
    fs::remove_dir(root).unwrap();
}

#[tokio::test]
async fn connect_rejects_an_empty_endpoint_before_local_or_network_side_effects() {
    assert_eq!(
        connect("  ", |_| {}).await.unwrap_err(),
        Error::Configuration(crate::ConfigError::EmptyEndpointId)
    );
}

#[test]
fn credential_store_rejects_relative_paths() {
    assert_eq!(
        prepare(Path::new("relative/auth.json")).unwrap_err(),
        Error::CredentialStoreUnavailable
    );
}

#[test]
fn credential_store_rejects_a_symbolic_link_auth_file() {
    use std::os::unix::fs::symlink;

    let path = temporary_auth_file("symlink");
    let parent = path.parent().unwrap();
    fs::create_dir_all(parent).unwrap();
    let target = parent.join("target.json");
    fs::write(&target, "{}").unwrap();
    symlink(&target, &path).unwrap();

    assert_eq!(
        prepare(&path).unwrap_err(),
        Error::CredentialStoreUnavailable
    );
    assert_eq!(fs::read_to_string(target).unwrap(), "{}");
    fs::remove_dir_all(temporary_root(&path)).unwrap();
}

#[test]
fn credential_store_rejects_a_symbolic_link_app_directory() {
    use std::os::unix::fs::symlink;

    let path = temporary_auth_file("parent-symlink");
    let root = temporary_root(&path);
    let redirected = root.join("redirected");
    fs::create_dir(&redirected).unwrap();
    symlink(&redirected, root.join("bone")).unwrap();

    assert_eq!(
        prepare(&path).unwrap_err(),
        Error::CredentialStoreUnavailable
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn credential_store_rejects_hard_linked_auth_files() {
    let path = temporary_auth_file("hardlink");
    drop(prepare(&path).unwrap());
    let copy = temporary_root(&path).join("auth-copy.json");
    fs::hard_link(&path, &copy).unwrap();

    assert_eq!(
        prepare(&path).unwrap_err(),
        Error::CredentialStoreUnavailable
    );
    assert_eq!(disconnect_at(&path), Err(Error::CredentialStoreUnavailable));
    fs::remove_dir_all(temporary_root(&path)).unwrap();
}

#[test]
fn service_errors_and_device_codes_are_redacted() {
    let error = Error::AuthorizationFailed;
    let rendered = format!("{error:?}: {error}");
    assert!(!rendered.contains("sentinel-secret-token"));
    assert!(!rendered.contains("Authorization: Bearer"));

    let prompt = DeviceCodePrompt {
        verification_uri: "https://auth.openai.com/codex/device".to_owned(),
        user_code: "SENTINEL-CODE".to_owned(),
    };
    let rendered = format!("{prompt:?}");
    assert!(rendered.contains("auth.openai.com"));
    assert!(!rendered.contains("SENTINEL-CODE"));
}

#[test]
fn credential_lease_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<CredentialLease>();
}
