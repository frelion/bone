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
use super::credential_store::{
    auth_file_path, credential_root_from, disconnect_at, disconnect_in, effective_uid, prepare,
    prepare_in,
};
use super::{DeviceCodePrompt, Error, LeasedModel, connect};
use crate::{Endpoint, Protocol, model::RequestSupport};

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
        "bone-llm-chatgpt-{case}-{}-{nonce}",
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
        .join("chatgpt-subscription")
        .join("auth.json")
}

fn temporary_root(path: &Path) -> &Path {
    path.parent()
        .and_then(Path::parent)
        .expect("test auth path should have a credential root")
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
fn managed_storage_is_scoped_to_the_injected_root() {
    let first_root = temporary_directory("first-injected-root");
    let second_root = temporary_directory("second-injected-root");
    let (first_path, first_lease) = prepare_in(&first_root).unwrap();
    let (second_path, second_lease) = prepare_in(&second_root).unwrap();
    drop((first_lease, second_lease));

    disconnect_in(&first_root).unwrap();
    assert!(!first_path.exists());
    assert!(second_path.exists());

    disconnect_in(&second_root).unwrap();
    fs::remove_dir_all(first_root).unwrap();
    fs::remove_dir_all(second_root).unwrap();
}

#[test]
fn default_credential_root_is_resolved_without_filesystem_side_effects() {
    let parent = temporary_directory("default-root");
    let xdg = parent.join("xdg");
    assert_eq!(
        credential_root_from(Some(xdg.clone().into()), None).unwrap(),
        xdg.join("bone")
    );
    assert!(!xdg.exists());

    let home = parent.join("home");
    fs::create_dir(&home).unwrap();
    for invalid_xdg in [OsString::new(), OsString::from("relative/config")] {
        let root = credential_root_from(Some(invalid_xdg), Some(home.clone().into()))
            .expect("invalid XDG values should fall back to HOME");
        assert_eq!(root, home.join(".config/bone"));
    }

    assert_eq!(
        credential_root_from(None, None),
        Err(Error::CredentialStoreUnavailable)
    );
    fs::remove_dir_all(parent).unwrap();
}

#[test]
fn injected_credential_roots_are_scoped_created_and_validated() {
    use std::os::unix::fs::PermissionsExt;

    let parent = temporary_directory("injected-root");
    let nested = parent.join("one").join("two").join("bone-data");
    let path = auth_file_path(&nested, true)
        .expect("owned nested credential roots should be created privately");
    assert_eq!(
        path,
        fs::canonicalize(&nested)
            .unwrap()
            .join("chatgpt-subscription/auth.json")
    );
    assert!(!nested.join("bone").exists());
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
        auth_file_path(&writable, true),
        Err(Error::CredentialStoreUnavailable)
    );

    let readable = parent.join("readable");
    fs::create_dir(&readable).unwrap();
    fs::set_permissions(&readable, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        auth_file_path(&readable, true),
        Err(Error::CredentialStoreUnavailable)
    );
    assert_eq!(
        disconnect_in(&readable),
        Err(Error::CredentialStoreUnavailable)
    );
    assert_eq!(
        fs::metadata(&readable).unwrap().permissions().mode() & 0o777,
        0o755,
        "the connector must not change an injected root"
    );

    let writable_parent = parent.join("writable-parent");
    let otherwise_private = writable_parent.join("private-root");
    fs::create_dir(&writable_parent).unwrap();
    fs::create_dir(&otherwise_private).unwrap();
    fs::set_permissions(&otherwise_private, fs::Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(&writable_parent, fs::Permissions::from_mode(0o777)).unwrap();
    assert_eq!(
        auth_file_path(&otherwise_private, true),
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
    let endpoint = Endpoint::from_model_factory_with_support(
        "chatgpt-test",
        Protocol::OpenAiResponses,
        RequestSupport::CHATGPT_SUBSCRIPTION,
        {
            let lease = Arc::clone(&lease);
            move |_model_id| LeasedModel {
                inner: FakeModel,
                _lease: Arc::clone(&lease),
            }
        },
    )
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
    let missing_root = root.join("missing-bone-root");
    assert_eq!(disconnect_in(&missing_root), Ok(()));
    assert!(!missing_root.exists());
    fs::remove_dir(root).unwrap();
}

#[tokio::test]
async fn connect_rejects_an_empty_endpoint_before_local_or_network_side_effects() {
    let parent = temporary_directory("invalid-endpoint");
    let credential_root = parent.join("must-not-be-created");
    assert_eq!(
        connect("  ", &credential_root, |_| {}).await.unwrap_err(),
        Error::Configuration(crate::ConfigError::EmptyEndpointId)
    );
    assert!(!credential_root.exists());
    fs::remove_dir(parent).unwrap();
}

#[tokio::test]
async fn connect_rejects_a_relative_credential_root_before_network_side_effects() {
    assert_eq!(
        connect("chatgpt-test", "relative/bone-root", |_| {})
            .await
            .unwrap_err(),
        Error::CredentialStoreUnavailable
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
fn credential_store_rejects_a_symbolic_link_service_directory() {
    use std::os::unix::fs::symlink;

    let path = temporary_auth_file("parent-symlink");
    let root = temporary_root(&path);
    let redirected = root.join("redirected");
    fs::create_dir(&redirected).unwrap();
    symlink(&redirected, root.join("chatgpt-subscription")).unwrap();

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
