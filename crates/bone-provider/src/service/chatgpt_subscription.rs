//! Experimental ChatGPT subscription access through the Codex Responses backend.
//!
//! This adapter uses Rig's in-process ChatGPT OAuth implementation. It does
//! not start a proxy or a Codex agent, and it is not the public OpenAI Platform
//! API. The explicit [`connect`] call may ask the user to complete a
//! device-code login; later requests reuse and refresh BONE's independent
//! ChatGPT token cache.
//!
//! Never point Rig's `auth_file` option at `~/.codex/auth.json`. Codex and Rig
//! use different file schemas and independent refresh-token lifecycles.

use std::{
    fmt::{self, Debug},
    path::PathBuf,
    sync::Arc,
};

#[cfg(any(unix, windows))]
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
};

#[cfg(any(unix, windows))]
use fs2::FileExt;
use rig_core::{
    client::CompletionClient,
    completion::{CompletionError, CompletionModel, CompletionRequest, CompletionResponse},
    http_client::HttpClientExt,
    providers::chatgpt as rig_chatgpt,
    streaming::StreamingCompletionResponse,
    wasm_compat::{WasmCompatSend, WasmCompatSync},
};

use crate::{ConfigError, Endpoint, error::validate_endpoint_id, protocol::openai_responses};

/// A redacted failure while explicitly connecting a ChatGPT subscription.
///
/// OAuth response bodies and tokens are deliberately not exposed through this
/// error boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectError {
    /// Local client or credential-store setup failed.
    Configuration(ConfigError),
    /// Interactive login, cached-token loading, or token refresh failed.
    AuthorizationFailed,
}

impl fmt::Display for ConnectError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Configuration(error) => fmt::Display::fmt(error, formatter),
            Self::AuthorizationFailed => formatter.write_str(
                "ChatGPT authorization failed; reconnect the subscription and try again",
            ),
        }
    }
}

impl std::error::Error for ConnectError {}

impl From<ConfigError> for ConnectError {
    fn from(error: ConfigError) -> Self {
        Self::Configuration(error)
    }
}

/// Device-code details for the application's explicit ChatGPT connection UI.
///
/// Treat the short code as ephemeral authentication material: display it only
/// in the active connection UI and do not log or persist it.
#[derive(Clone, PartialEq, Eq)]
pub struct DeviceCodePrompt {
    pub verification_uri: String,
    pub user_code: String,
}

impl Debug for DeviceCodePrompt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeviceCodePrompt")
            .field("verification_uri", &self.verification_uri)
            .field("user_code", &"<redacted>")
            .finish()
    }
}

/// Explicitly connect an in-process ChatGPT subscription endpoint.
///
/// No API key, sidecar, or local HTTP proxy is required. On native targets Rig
/// stores its OAuth record in BONE's application-owned config directory. This
/// call authorizes before returning, so a later model request never surprises
/// the caller by starting a device-code flow. OAuth is unsupported on WASM.
pub async fn connect<F>(
    endpoint_id: impl Into<String>,
    on_device_code: F,
) -> Result<Endpoint, ConnectError>
where
    F: Fn(DeviceCodePrompt) + Send + Sync + 'static,
{
    let endpoint_id = endpoint_id.into();
    validate_endpoint_id(&endpoint_id)?;
    let auth_file = auth_file_path()?;
    let lease = Arc::new(prepare_credential_store(&auth_file)?);
    let interactive_client = rig_chatgpt::Client::builder()
        .oauth()
        .auth_file(&auth_file)
        .allow_device_flow(true)
        .on_device_code(move |prompt| {
            on_device_code(DeviceCodePrompt {
                verification_uri: prompt.verification_uri,
                user_code: prompt.user_code,
            });
        })
        // A provider foundation must not silently prepend an assistant persona.
        .default_instructions("")
        .originator("bone")
        .user_agent(user_agent())
        .build()
        .map_err(|_| ConfigError::InvalidClientConfiguration)?;

    interactive_client
        .authorize()
        .await
        .map_err(|_| ConnectError::AuthorizationFailed)?;

    // The endpoint gets a new non-interactive client. If a later refresh is
    // rejected, an ordinary model request fails instead of printing a code or
    // waiting for device authorization.
    let client = rig_chatgpt::Client::builder()
        .oauth()
        .auth_file(auth_file)
        .allow_device_flow(false)
        .default_instructions("")
        .originator("bone")
        .user_agent(user_agent())
        .build()
        .map_err(|_| ConfigError::InvalidClientConfiguration)?;

    openai_responses::from_completion_model_factory(endpoint_id, move |model_id| LeasedModel {
        inner: client.completion_model(model_id),
        _lease: Arc::clone(&lease),
    })
    .map_err(Into::into)
}

/// Wrap a configured Rig ChatGPT client.
///
/// Use this entry point when an application needs a custom device-code UI,
/// an app-owned token path, a custom transport, or `allow_device_flow(false)`
/// for non-interactive services. Authentication and service behavior stay in
/// Rig; BONE only assigns endpoint and Responses protocol identities. The
/// caller owns credential permissions and cross-process coordination for a
/// custom client; the guarded defaults of [`connect`] do not apply here. Use
/// `from_client` does not authorize or change interaction policy. If ordinary
/// requests must never enter device flow, first authorize an interactive
/// client, then pass a client rebuilt over the same app-owned cache with
/// `allow_device_flow(false)`.
pub fn from_client<H>(
    endpoint_id: impl Into<String>,
    client: rig_chatgpt::Client<H>,
) -> Result<Endpoint, ConfigError>
where
    H: HttpClientExt
        + Clone
        + Default
        + Debug
        + WasmCompatSend
        + WasmCompatSync
        + Send
        + Sync
        + 'static,
{
    openai_responses::from_completion_model_factory(endpoint_id, move |model_id| {
        client.completion_model(model_id)
    })
}

/// Delete BONE's independent local ChatGPT credential record.
///
/// This disconnects future BONE clients but does not revoke the upstream
/// ChatGPT session. Existing endpoint handles should be dropped by the caller.
pub fn disconnect() -> Result<(), ConfigError> {
    let path = auth_file_path()?;
    disconnect_at(&path)
}

#[cfg(any(unix, windows))]
fn disconnect_at(path: &std::path::Path) -> Result<(), ConfigError> {
    let service_dir = path
        .parent()
        .ok_or(ConfigError::CredentialStoreUnavailable)?;
    match fs::symlink_metadata(service_dir) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(ConfigError::CredentialStoreUnavailable),
        Ok(_) => {}
    }
    let _lease = acquire_credential_store(path)?;
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err(ConfigError::CredentialStoreUnavailable)
        }
        Ok(_) => {
            drop(secure_open(path, false)?);
            fs::remove_file(path).map_err(|_| ConfigError::CredentialStoreUnavailable)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(ConfigError::CredentialStoreUnavailable),
    }
}

#[cfg(not(any(unix, windows)))]
fn disconnect_at(_path: &std::path::Path) -> Result<(), ConfigError> {
    Err(ConfigError::CredentialStoreUnavailable)
}

fn auth_file_path() -> Result<PathBuf, ConfigError> {
    #[cfg(any(target_os = "windows", not(any(unix, windows))))]
    return Err(ConfigError::CredentialStoreUnavailable);

    #[cfg(all(not(target_os = "windows"), any(unix, windows)))]
    let root = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));

    #[cfg(all(not(target_os = "windows"), any(unix, windows)))]
    {
        let root = root
            .filter(|path| path.is_absolute())
            .ok_or(ConfigError::CredentialStoreUnavailable)?;
        let root = fs::canonicalize(root).map_err(|_| ConfigError::CredentialStoreUnavailable)?;
        Ok(root
            .join("bone")
            .join("chatgpt-subscription")
            .join("auth.json"))
    }
}

#[cfg(any(unix, windows))]
fn prepare_credential_store(path: &std::path::Path) -> Result<CredentialLease, ConfigError> {
    let lease = acquire_credential_store(path)?;

    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(ConfigError::CredentialStoreUnavailable);
        }
        Ok(_) => drop(secure_open(path, false)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut file = secure_open(path, true)?;
            file.write_all(b"{}")
                .and_then(|_| file.sync_all())
                .map_err(|_| ConfigError::CredentialStoreUnavailable)?;
        }
        Err(_) => return Err(ConfigError::CredentialStoreUnavailable),
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .map_err(|_| ConfigError::CredentialStoreUnavailable)?;
    }

    Ok(lease)
}

#[cfg(not(any(unix, windows)))]
fn prepare_credential_store(_path: &std::path::Path) -> Result<CredentialLease, ConfigError> {
    Err(ConfigError::CredentialStoreUnavailable)
}

#[cfg(any(unix, windows))]
fn acquire_credential_store(path: &std::path::Path) -> Result<CredentialLease, ConfigError> {
    if !path.is_absolute() {
        return Err(ConfigError::CredentialStoreUnavailable);
    }
    let service_dir = path
        .parent()
        .ok_or(ConfigError::CredentialStoreUnavailable)?;
    let app_dir = service_dir
        .parent()
        .ok_or(ConfigError::CredentialStoreUnavailable)?;
    let config_root = app_dir
        .parent()
        .ok_or(ConfigError::CredentialStoreUnavailable)?;
    ensure_existing_directory(config_root)?;
    ensure_private_directory(app_dir)?;
    ensure_private_directory(service_dir)?;

    let lock_path = service_dir.join("auth.lock");
    let lock_file = secure_open(&lock_path, false)?;
    lock_file
        .try_lock_exclusive()
        .map_err(|_| ConfigError::CredentialStoreUnavailable)?;
    Ok(CredentialLease { _file: lock_file })
}

#[cfg(any(unix, windows))]
fn ensure_existing_directory(path: &std::path::Path) -> Result<(), ConfigError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|_| ConfigError::CredentialStoreUnavailable)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ConfigError::CredentialStoreUnavailable);
    }
    Ok(())
}

#[cfg(any(unix, windows))]
fn ensure_private_directory(path: &std::path::Path) -> Result<(), ConfigError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(ConfigError::CredentialStoreUnavailable);
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path).map_err(|_| ConfigError::CredentialStoreUnavailable)?;
        }
        Err(_) => return Err(ConfigError::CredentialStoreUnavailable),
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|_| ConfigError::CredentialStoreUnavailable)?;
    }
    Ok(())
}

#[cfg(any(unix, windows))]
fn secure_open(path: &std::path::Path, create_new: bool) -> Result<File, ConfigError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(ConfigError::CredentialStoreUnavailable);
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(ConfigError::CredentialStoreUnavailable),
    }

    let mut options = OpenOptions::new();
    options.read(true).write(true);
    if create_new {
        options.create_new(true);
    } else {
        options.create(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options
        .open(path)
        .map_err(|_| ConfigError::CredentialStoreUnavailable)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let metadata = file
            .metadata()
            .map_err(|_| ConfigError::CredentialStoreUnavailable)?;
        let parent_metadata = path
            .parent()
            .and_then(|parent| fs::metadata(parent).ok())
            .ok_or(ConfigError::CredentialStoreUnavailable)?;
        if metadata.nlink() != 1 || metadata.uid() != parent_metadata.uid() {
            return Err(ConfigError::CredentialStoreUnavailable);
        }
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|_| ConfigError::CredentialStoreUnavailable)?;
    }

    Ok(file)
}

#[cfg(any(unix, windows))]
#[derive(Debug)]
struct CredentialLease {
    _file: File,
}

#[cfg(not(any(unix, windows)))]
#[derive(Debug)]
struct CredentialLease;

#[derive(Clone)]
struct LeasedModel<M> {
    inner: M,
    _lease: Arc<CredentialLease>,
}

impl<M> CompletionModel for LeasedModel<M>
where
    M: CompletionModel + Send + Sync,
{
    async fn completion(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, CompletionError> {
        self.inner
            .completion(request)
            .await
            .map_err(sanitize_request_error)
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<StreamingCompletionResponse, CompletionError> {
        self.inner
            .stream(request)
            .await
            .map_err(sanitize_request_error)
    }

    fn capabilities(&self) -> rig_core::completion::ProviderCapabilities {
        self.inner.capabilities()
    }
}

fn sanitize_request_error(error: CompletionError) -> CompletionError {
    match error {
        CompletionError::ProviderError(_) => CompletionError::ProviderError(
            "ChatGPT authorization or provider setup failed; reconnect and try again".to_owned(),
        ),
        error => error,
    }
}

fn user_agent() -> String {
    format!(
        "bone-provider/{} ({} {})",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

#[cfg(all(test, any(unix, windows)))]
mod tests {
    use std::{
        io::{BufRead, BufReader},
        process::{Command, Stdio},
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    use super::*;

    #[derive(Clone)]
    struct FakeModel;

    #[derive(Clone)]
    struct SecretErrorModel;

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

    impl CompletionModel for SecretErrorModel {
        async fn completion(
            &self,
            _request: CompletionRequest,
        ) -> Result<CompletionResponse, CompletionError> {
            Err(CompletionError::ProviderError(
                "sentinel-secret-oauth-body".to_owned(),
            ))
        }

        async fn stream(
            &self,
            _request: CompletionRequest,
        ) -> Result<StreamingCompletionResponse, CompletionError> {
            Err(CompletionError::ProviderError(
                "sentinel-secret-oauth-body".to_owned(),
            ))
        }
    }

    fn temporary_auth_file(case: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should follow the Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "bone-provider-chatgpt-{case}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("test config root should be created");
        root.join("bone")
            .join("chatgpt-subscription")
            .join("auth.json")
    }

    fn temporary_root(path: &std::path::Path) -> &std::path::Path {
        path.parent()
            .and_then(std::path::Path::parent)
            .and_then(std::path::Path::parent)
            .expect("test auth path should have a config root")
    }

    fn request() -> CompletionRequest {
        CompletionRequest {
            model: None,
            preamble: None,
            chat_history: vec![rig_core::message::Message::user("hello")],
            documents: Vec::new(),
            tools: Vec::new(),
            temperature: None,
            max_tokens: None,
            tool_choice: None,
            additional_params: None,
            output_schema: None,
            record_telemetry_content: false,
        }
    }

    #[test]
    fn credential_store_is_private_and_single_owner() {
        let path = temporary_auth_file("private");
        let parent = path.parent().expect("test path should have a parent");
        let first = prepare_credential_store(&path).expect("first owner should acquire the store");
        assert_eq!(fs::read_to_string(&path).unwrap(), "{}");
        assert_eq!(
            prepare_credential_store(&path).unwrap_err(),
            ConfigError::CredentialStoreUnavailable
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
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
        }

        drop(first);
        let second = prepare_credential_store(&path).expect("released store should be reusable");
        drop(second);
        fs::remove_dir_all(temporary_root(&path))
            .expect("test credential directory should be removable");
    }

    #[test]
    fn credential_lock_child() {
        let Some(path) = std::env::var_os("BONE_TEST_CHATGPT_LOCK_PATH") else {
            return;
        };
        let _lease = prepare_credential_store(std::path::Path::new(&path))
            .expect("child process should acquire credential lease");
        println!("BONE_TEST_LOCKED");
        std::io::stdout()
            .flush()
            .expect("child lock signal should flush");
        std::thread::sleep(Duration::from_secs(60));
    }

    #[test]
    fn credential_store_lock_is_exclusive_across_processes_and_released_on_exit() {
        let path = temporary_auth_file("subprocess");
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "service::chatgpt_subscription::tests::credential_lock_child",
                "--nocapture",
            ])
            .env("BONE_TEST_CHATGPT_LOCK_PATH", &path)
            .stdout(Stdio::piped())
            .spawn()
            .expect("lock-holder child should start");
        let stdout = child.stdout.take().expect("child stdout should be piped");
        let mut lines = BufReader::new(stdout).lines();
        let mut locked = false;
        for _ in 0..20 {
            let Some(line) = lines.next() else {
                break;
            };
            if line.unwrap_or_default().contains("BONE_TEST_LOCKED") {
                locked = true;
                break;
            }
        }
        assert!(locked, "child should report an acquired OS lock");
        assert_eq!(
            prepare_credential_store(&path).unwrap_err(),
            ConfigError::CredentialStoreUnavailable
        );

        child.kill().expect("lock-holder child should be killable");
        child.wait().expect("lock-holder child should be reapable");
        let lease = prepare_credential_store(&path)
            .expect("OS should release the credential lock when its process exits");
        drop(lease);
        fs::remove_dir_all(temporary_root(&path))
            .expect("test credential directory should be removable");
    }

    #[test]
    fn endpoint_and_selected_model_hold_the_store_lease() {
        let path = temporary_auth_file("lifetime");
        let lease = Arc::new(prepare_credential_store(&path).unwrap());
        let endpoint = openai_responses::from_completion_model_factory("chatgpt-test", {
            let lease = Arc::clone(&lease);
            move |_model_id| LeasedModel {
                inner: FakeModel,
                _lease: Arc::clone(&lease),
            }
        })
        .unwrap();
        drop(lease);

        assert_eq!(
            disconnect_at(&path),
            Err(ConfigError::CredentialStoreUnavailable)
        );
        let model = endpoint.model("gpt-test").unwrap();
        drop(endpoint);
        assert_eq!(
            disconnect_at(&path),
            Err(ConfigError::CredentialStoreUnavailable)
        );
        drop(model);
        assert_eq!(disconnect_at(&path), Ok(()));
        fs::remove_dir_all(temporary_root(&path))
            .expect("test credential directory should be removable");
    }

    #[test]
    fn disconnect_without_a_cache_creates_no_app_artifacts() {
        let path = temporary_auth_file("never-connected");
        let root = temporary_root(&path).to_path_buf();
        assert_eq!(disconnect_at(&path), Ok(()));
        assert!(!root.join("bone").exists());
        fs::remove_dir(root).expect("empty test config root should be removable");
    }

    #[tokio::test]
    async fn connect_rejects_an_empty_endpoint_before_local_or_network_side_effects() {
        assert_eq!(
            connect("  ", |_| {}).await.unwrap_err(),
            ConnectError::Configuration(ConfigError::EmptyEndpointId)
        );
    }

    #[tokio::test]
    async fn request_time_provider_errors_are_redacted_for_unary_and_stream() {
        let path = temporary_auth_file("redaction");
        let lease = Arc::new(prepare_credential_store(&path).unwrap());
        let model = LeasedModel {
            inner: SecretErrorModel,
            _lease: lease,
        };

        let unary = model.completion(request()).await.unwrap_err();
        let stream = match model.stream(request()).await {
            Ok(_) => panic!("secret-bearing provider error unexpectedly succeeded"),
            Err(error) => error,
        };
        for error in [unary, stream] {
            let rendered = format!("{error:?}: {error}");
            assert!(!rendered.contains("sentinel-secret-oauth-body"));
            assert!(rendered.contains("reconnect"));
        }
        drop(model);
        fs::remove_dir_all(temporary_root(&path))
            .expect("test credential directory should be removable");
    }

    #[test]
    fn credential_store_rejects_relative_paths() {
        assert_eq!(
            prepare_credential_store(std::path::Path::new("relative/auth.json")).unwrap_err(),
            ConfigError::CredentialStoreUnavailable
        );
    }

    #[cfg(unix)]
    #[test]
    fn credential_store_rejects_a_symbolic_link_auth_file() {
        use std::os::unix::fs::symlink;

        let path = temporary_auth_file("symlink");
        let parent = path.parent().expect("test path should have a parent");
        fs::create_dir_all(parent).unwrap();
        let target = parent.join("target.json");
        fs::write(&target, "{}").unwrap();
        symlink(&target, &path).unwrap();

        assert_eq!(
            prepare_credential_store(&path).unwrap_err(),
            ConfigError::CredentialStoreUnavailable
        );
        assert_eq!(fs::read_to_string(target).unwrap(), "{}");
        fs::remove_dir_all(temporary_root(&path))
            .expect("test credential directory should be removable");
    }

    #[cfg(unix)]
    #[test]
    fn credential_store_rejects_a_symbolic_link_app_directory() {
        use std::os::unix::fs::symlink;

        let path = temporary_auth_file("parent-symlink");
        let root = temporary_root(&path);
        let redirected = root.join("redirected");
        fs::create_dir(&redirected).unwrap();
        symlink(&redirected, root.join("bone")).unwrap();

        assert_eq!(
            prepare_credential_store(&path).unwrap_err(),
            ConfigError::CredentialStoreUnavailable
        );
        fs::remove_dir_all(root).expect("test config root should be removable");
    }

    #[cfg(unix)]
    #[test]
    fn credential_store_rejects_hard_linked_auth_files() {
        let path = temporary_auth_file("hardlink");
        let lease = prepare_credential_store(&path).unwrap();
        drop(lease);
        let copy = temporary_root(&path).join("auth-copy.json");
        fs::hard_link(&path, &copy).unwrap();

        assert_eq!(
            prepare_credential_store(&path).unwrap_err(),
            ConfigError::CredentialStoreUnavailable
        );
        assert_eq!(
            disconnect_at(&path),
            Err(ConfigError::CredentialStoreUnavailable)
        );
        fs::remove_dir_all(temporary_root(&path))
            .expect("test credential directory should be removable");
    }

    #[test]
    fn connect_errors_are_redacted() {
        let error = ConnectError::AuthorizationFailed;
        let rendered = format!("{error:?}: {error}");
        assert!(!rendered.contains("sentinel-secret-token"));
        assert!(!rendered.contains("Authorization: Bearer"));
    }

    #[test]
    fn device_code_debug_is_redacted() {
        let prompt = DeviceCodePrompt {
            verification_uri: "https://auth.openai.com/codex/device".to_owned(),
            user_code: "SENTINEL-CODE".to_owned(),
        };
        let rendered = format!("{prompt:?}");
        assert!(rendered.contains("auth.openai.com"));
        assert!(!rendered.contains("SENTINEL-CODE"));
    }
}
