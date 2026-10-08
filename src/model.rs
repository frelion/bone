//! Native Rig model construction and BONE-owned subscription credential lifetimes.
use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use fs2::FileExt;
use rig_core::{
    driver::DynModel,
    error::ProviderError,
    http_client::DynHttpClient,
    operation::Completion,
    providers::{
        chatgpt::auth::{AuthError, AuthSource, Authenticator, DeviceCodeHandler},
        registry::{ProviderConfig, ProviderId},
    },
};
use sha2::{Digest, Sha256};

use crate::config::{ModelReference, Profile, validate_profile_name};

/// Keep this owner alive until the native call or stream is completely finished.
/// The private guard serializes subscription refresh AND inference across processes.
pub struct ModelConnection {
    pub model: DynModel<Completion>,
    _credential_lock: Option<CredentialLock>,
}

/// A credential lease acquired without making a provider request. Runtime waits
/// for this cancellable stage before starting the connection/inference deadline.
pub struct PreparedModel {
    profile: Profile,
    data_dir: PathBuf,
    profile_name: String,
    http: DynHttpClient,
    auth: Option<Authenticator>,
    credential_lock: Option<CredentialLock>,
    api_key: Option<String>,
}

struct CredentialLock(File);

impl Drop for CredentialLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

#[cfg(test)]
pub async fn connect(
    profile: &Profile,
    data_dir: &Path,
    profile_name: &str,
    job_id: &str,
    call_id: &str,
) -> Result<ModelConnection> {
    prepare(profile, data_dir, profile_name, job_id, call_id)
        .await?
        .connect()
        .await
}

/// Acquire only local state. Authentication starts in `connect`, after the
/// runtime's cancellable credential wait and before its inference deadline.
pub async fn prepare(
    profile: &Profile,
    data_dir: &Path,
    profile_name: &str,
    job_id: &str,
    call_id: &str,
) -> Result<PreparedModel> {
    ensure!(
        !job_id.trim().is_empty() && !call_id.trim().is_empty(),
        "every model invocation requires a JobId and CallId"
    );
    validate_profile_name(profile_name)?;
    profile.validate()?;
    let (credential_lock, auth) = if profile.is_subscription() {
        let (guard, auth_file, source) = if profile.reuse_codex_login {
            let (guard, canonical_source) = lock_codex_source(
                &codex_auth_file()?,
                &crate::config::user_home()?.join(".bone/v2/credential-locks"),
            )
            .await?;
            (guard, None, load_codex_login(&canonical_source)?)
        } else {
            let (guard, auth_file) = lock_profile(data_dir, profile_name).await?;
            (guard, Some(auth_file), AuthSource::OAuth)
        };
        let auth = Authenticator::new(source, auth_file, DeviceCodeHandler::new(|_| {}), false);
        (Some(guard), Some(auth))
    } else {
        (None, None)
    };
    let api_key = if profile.is_subscription() {
        None
    } else {
        resolve_api_key(profile, data_dir, profile_name)?
    };
    Ok(PreparedModel {
        profile: profile.clone(),
        data_dir: data_dir.to_owned(),
        profile_name: profile_name.to_owned(),
        http: DynHttpClient::new(rig_reqwest::shared()),
        auth,
        credential_lock,
        api_key,
    })
}

impl PreparedModel {
    pub async fn connect(self) -> Result<ModelConnection> {
        let Self {
            profile,
            data_dir,
            profile_name,
            http,
            auth,
            credential_lock,
            api_key,
        } = self;
        let model = match &profile.model {
            ModelReference::Registry(reference) if profile.is_subscription() => {
                let ProviderConfig::OpenAi(config) = reference.config("") else {
                    bail!("ChatGPT requires its native OpenAI dialect")
                };
                let auth = auth.context("subscription was not prepared")?;
                let client = config
                    .connect(http)
                    .authenticate(&auth)
                    .await
                    .map_err(|error| authentication_error(&profile_name, error))?;
                if !profile.reuse_codex_login {
                    restrict_cache_permissions(&self::auth_file(&data_dir, &profile_name)?)?;
                }
                client.completion(reference.model()).erase()
            }
            ModelReference::Registry(reference) => {
                if let Some(key) = api_key {
                    reference.completion_model_with(key, http)
                } else {
                    reference.completion_model().map_err(environment_error)?
                }
            }
            ModelReference::Cohere { cohere, model } => {
                let mut config = cohere.clone();
                config.api_key = api_key.unwrap_or_default().into();
                config.connect(http).completion(model).erase()
            }
            ModelReference::Ollama { ollama, model } => {
                let mut config = ollama.clone();
                config.api_key = api_key.unwrap_or_default().into();
                config.connect(http).completion(model).erase()
            }
            ModelReference::Bedrock { bedrock } => bedrock_model(bedrock)?,
            ModelReference::VertexAi { vertexai } => vertexai_model(vertexai)?,
            ModelReference::Candle { candle } => candle_model(candle).await?,
        };
        Ok(ModelConnection {
            model,
            _credential_lock: credential_lock,
        })
    }
}

fn environment_error(error: rig_core::client::env::EnvError) -> anyhow::Error {
    use rig_core::client::env::EnvError;
    let name = match error {
        EnvError::Variable { name, .. } | EnvError::Invalid { name, .. } => name,
    };
    anyhow::anyhow!("provider environment variable `{name}` is missing or invalid")
}

fn read_credential(name: &str, required: bool) -> Result<String> {
    match std::env::var(name) {
        Ok(value) if !required || !value.is_empty() => Ok(value),
        Err(std::env::VarError::NotPresent) if !required => Ok(String::new()),
        _ => bail!("credential variable `{name}` is missing or invalid"),
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ApiKeyRecord {
    provider: String,
    endpoint: Option<String>,
    key: String,
}

pub fn api_key_file(data_dir: &Path, profile_name: &str) -> Result<PathBuf> {
    validate_profile_name(profile_name)?;
    let path = data_dir.join("profiles").join(profile_name).join("api-key");
    for component in [
        data_dir.join("profiles"),
        data_dir.join("profiles").join(profile_name),
        path.clone(),
    ] {
        match std::fs::symlink_metadata(&component) {
            Ok(metadata) => ensure!(
                !metadata.file_type().is_symlink(),
                "BONE credential path must not be a symbolic link"
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => bail!("cannot inspect BONE credential path"),
        }
    }
    Ok(path)
}

pub fn save_api_key(
    data_dir: &Path,
    profile_name: &str,
    profile: &Profile,
    key: &str,
) -> Result<()> {
    profile.validate()?;
    ensure!(
        !profile.is_subscription() && profile.endpoint().is_some(),
        "this native connection does not use a profile API key"
    );
    ensure!(
        !key.trim().is_empty() && key.len() <= 1024 * 1024 && !key.chars().any(char::is_control),
        "API key must be nonblank and contain no control characters"
    );
    let path = api_key_file(data_dir, profile_name)?;
    let directory = path.parent().context("credential directory is missing")?;
    std::fs::create_dir_all(directory).context("cannot create BONE credential directory")?;
    crate::filesystem::private_directory(directory)?;
    // Recheck after directory creation; never follow an existing profile/key alias.
    let path = api_key_file(data_dir, profile_name)?;
    let record = ApiKeyRecord {
        provider: profile.provider_identity(),
        endpoint: profile.endpoint(),
        key: key.into(),
    };
    let bytes = serde_json::to_vec(&record)?;
    ensure!(
        bytes.len() <= 1024 * 1024,
        "BONE API key file exceeds the supported size"
    );
    crate::config::write_atomic(&path, &bytes, None)
        .map_err(|_| anyhow::anyhow!("cannot save BONE profile API key"))
}

pub fn remove_api_key(data_dir: &Path, profile_name: &str) -> Result<()> {
    let path = api_key_file(data_dir, profile_name)?;
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => bail!("cannot remove BONE profile API key"),
    }
}

/// Presence only. Never reads key/token text or contacts a provider.
pub fn has_api_key(data_dir: &Path, profile_name: &str) -> Result<bool> {
    local_file_present(&api_key_file(data_dir, profile_name)?)
}

/// Cache presence does not claim the token is valid or still authorized.
pub fn has_login(profile: &Profile, data_dir: &Path, profile_name: &str) -> Result<bool> {
    ensure!(
        profile.is_subscription(),
        "connection is not a ChatGPT subscription"
    );
    validate_profile_name(profile_name)?;
    local_file_present(&if profile.reuse_codex_login {
        codex_auth_file()?
    } else {
        auth_file(data_dir, profile_name)?
    })
}

fn local_file_present(path: &Path) -> Result<bool> {
    match std::fs::metadata(path) {
        Ok(metadata) => Ok(metadata.is_file() && metadata.len() > 0),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => bail!("cannot inspect local credential file"),
    }
}

fn resolve_api_key(
    profile: &Profile,
    data_dir: &Path,
    profile_name: &str,
) -> Result<Option<String>> {
    let required = match &profile.model {
        ModelReference::Registry(reference) => {
            reference.id().is_none_or(|id| id.requires_credential())
        }
        ModelReference::Cohere { .. } => true,
        ModelReference::Ollama { .. } => false,
        _ => return Ok(None), // These providers own their native credential chains.
    };
    if let Some(name) = &profile.credential_env {
        return read_credential(name, required).map(Some);
    }
    let path = api_key_file(data_dir, profile_name)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    match options.open(path) {
        Ok(mut file) => {
            let metadata = file.metadata().context("cannot inspect BONE API key")?;
            #[cfg(windows)]
            crate::filesystem::check_private(&file)?;
            ensure!(
                metadata.is_file() && metadata.len() <= 1024 * 1024,
                "BONE API key file is invalid"
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                ensure!(
                    metadata.permissions().mode() & 0o077 == 0,
                    "BONE API key file must have private permissions (0600)"
                );
            }
            use std::io::Read;
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)
                .map_err(|_| anyhow::anyhow!("cannot read BONE API key"))?;
            let record: ApiKeyRecord = serde_json::from_slice(&bytes).map_err(|_| {
                anyhow::anyhow!("BONE API key file is invalid; provide the key again")
            })?;
            ensure!(
                record.provider == profile.provider_identity()
                    && record.endpoint == profile.endpoint(),
                "saved API key belongs to a different provider or endpoint; provide a key for this connection"
            );
            ensure!(
                !record.key.trim().is_empty() && !record.key.chars().any(char::is_control),
                "BONE API key file is invalid"
            );
            Ok(Some(record.key))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => match &profile.model {
            ModelReference::Cohere { .. } => read_credential("COHERE_API_KEY", true).map(Some),
            ModelReference::Ollama { .. } => read_credential("OLLAMA_API_KEY", false).map(Some),
            _ => Ok(None), // Rig resolves its own default environment without mutation.
        },
        Err(_) => bail!("cannot read BONE API key"),
    }
}

struct LoginStaging(PathBuf);
impl Drop for LoginStaging {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Explicit sign-in starts a fresh native device flow. Replace the current cache
/// only after successful authentication, so an interrupted login preserves it.
pub async fn login(profile: &Profile, data_dir: &Path, profile_name: &str) -> Result<()> {
    login_with(
        profile,
        data_dir,
        profile_name,
        DynHttpClient::new(rig_reqwest::shared()),
        DeviceCodeHandler::default(),
    )
    .await
}

pub async fn login_with(
    profile: &Profile,
    data_dir: &Path,
    profile_name: &str,
    http: DynHttpClient,
    handler: DeviceCodeHandler,
) -> Result<()> {
    ensure!(
        profile.is_subscription(),
        "login requires a ChatGPT subscription profile"
    );
    if profile.reuse_codex_login {
        validate_profile_name(profile_name)?;
        load_codex_login(&codex_auth_file()?)?;
        return Ok(());
    }
    let (_guard, auth_file) = lock_profile(data_dir, profile_name).await?;
    let staging = auth_file.with_file_name(format!("auth-login-{}.json", uuid::Uuid::new_v4()));
    let _staging_cleanup = LoginStaging(staging.clone());
    let ModelReference::Registry(reference) = &profile.model else {
        unreachable!()
    };
    let ProviderConfig::OpenAi(config) = reference.config("") else {
        unreachable!()
    };
    let auth = Authenticator::new(AuthSource::OAuth, Some(staging.clone()), handler, true);
    let result = config.connect(http).authenticate(&auth).await;
    match result {
        Ok(_) => {
            restrict_cache_permissions(&staging)?;
            if let Err(error) = std::fs::rename(&staging, &auth_file) {
                let _ = std::fs::remove_file(&staging);
                return Err(error).context("cannot install BONE subscription cache");
            }
            Ok(())
        }
        Err(error) => {
            let _ = std::fs::remove_file(&staging);
            Err(authentication_error(profile_name, error))
        }
    }
}

fn codex_auth_file() -> Result<PathBuf> {
    let directory = if let Some(path) = std::env::var_os("CODEX_HOME").filter(|p| !p.is_empty()) {
        PathBuf::from(path)
    } else {
        PathBuf::from(
            std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                .context("cannot find Codex login directory")?,
        )
        .join(".codex")
    };
    Ok(directory.join("auth.json"))
}

fn load_codex_login(path: &Path) -> Result<AuthSource> {
    #[derive(serde::Deserialize)]
    struct Login {
        tokens: Option<Tokens>,
    }
    #[derive(serde::Deserialize)]
    struct Tokens {
        access_token: Option<String>,
        account_id: Option<String>,
    }
    // Project only the public authenticator inputs. Never extract, transform,
    // refresh, serialize or write Codex's refresh/id tokens.
    let bytes = std::fs::read(path).map_err(|_| {
        anyhow::anyhow!(
            "Codex subscription login cannot be read; sign in through Codex and try again"
        )
    })?;
    let login: Login = serde_json::from_slice(&bytes).map_err(|_| {
        anyhow::anyhow!(
            "Codex subscription login is invalid; refresh the Codex login and try again"
        )
    })?;
    let tokens = login
        .tokens
        .context("Codex subscription login is missing; sign in through Codex and try again")?;
    let access_token = tokens
        .access_token
        .filter(|token| !token.is_empty())
        .context(
            "Codex subscription access token is missing; refresh the Codex login and try again",
        )?;
    Ok(AuthSource::AccessToken {
        access_token,
        account_id: tokens.account_id.filter(|id| !id.is_empty()),
    })
}

fn restrict_cache_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .context("cannot restrict BONE subscription cache permissions")?;
    }
    #[cfg(windows)]
    {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        crate::filesystem::private_file(&file)?;
    }
    Ok(())
}

pub fn auth_file(data_dir: &Path, profile_name: &str) -> Result<PathBuf> {
    validate_profile_name(profile_name)?;
    Ok(data_dir
        .join("profiles")
        .join(profile_name)
        .join("auth.json"))
}

async fn lock_profile(data_dir: &Path, profile_name: &str) -> Result<(CredentialLock, PathBuf)> {
    let auth_file = auth_file(data_dir, profile_name)?;
    let guard = acquire_credential_lock(&auth_file.with_file_name("auth.lock")).await?;
    Ok((guard, auth_file))
}

async fn lock_codex_source(
    source: &Path,
    lock_directory: &Path,
) -> Result<(CredentialLock, PathBuf)> {
    let canonical_source = source.canonicalize().map_err(|_| {
        anyhow::anyhow!(
            "Codex subscription login cannot be read; sign in through Codex and try again"
        )
    })?;
    let identity = format!(
        "{:x}",
        Sha256::digest(canonical_source.as_os_str().as_encoded_bytes())
    );
    std::fs::create_dir_all(lock_directory)
        .context("cannot create BONE subscription lock directory")?;
    let lock = lock_directory
        .canonicalize()?
        .join(format!("{identity}.lock"));
    // All profiles and data directories share the canonical source's lease in
    // the OS-user home, independent of HOME and CODEX_HOME aliases.
    let guard = acquire_credential_lock(&lock).await?;
    Ok((guard, canonical_source))
}

async fn acquire_credential_lock(path: &Path) -> Result<CredentialLock> {
    let directory = path.parent().context("subscription lock parent missing")?;
    std::fs::create_dir_all(directory).context("cannot create BONE subscription lock directory")?;
    crate::filesystem::private_directory(directory)?;
    let mut options = OpenOptions::new();
    options.create(true).truncate(false).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(path)
        .context("cannot open BONE subscription lock")?;
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(CredentialLock(file)),
            Err(error) if crate::filesystem::lock_contended(&error) => {
                // This future owns the unopened lease candidate. Cancellation
                // closes it immediately; no blocking worker survives the call.
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            Err(error) => return Err(error).context("cannot acquire BONE subscription lock"),
        }
    }
}

fn authentication_error(profile_name: &str, error: AuthError) -> anyhow::Error {
    let reason = match error {
        AuthError::Json(_) => "subscription cache is invalid",
        AuthError::Io(_) => "subscription cache cannot be read or written",
        AuthError::Http(_) => "subscription authentication failed",
        AuthError::Message(_) => "subscription sign-in is required",
    };
    // Do not attach a source chain: OAuth responses/cache parsing may contain
    // credentials. The SDK alone interprets the credential record.
    anyhow::anyhow!("{reason}; run `bone login --profile '{profile_name}'` to sign in again")
}

pub fn call_error(profile_name: &str, error: ProviderError) -> anyhow::Error {
    if error.report().http_status == Some(401) {
        anyhow::anyhow!(
            "provider rejected authentication (401); run `bone login --profile '{profile_name}'` for subscription profiles or update the configured credential"
        )
    } else {
        error.into()
    }
}

pub fn call_error_for_profile(
    profile: &Profile,
    profile_name: &str,
    error: ProviderError,
) -> anyhow::Error {
    if profile.reuse_codex_login && error.report().http_status == Some(401) {
        anyhow::anyhow!(
            "Codex subscription login was rejected (401); refresh the Codex login and run BONE again"
        )
    } else {
        call_error(profile_name, error)
    }
}

/// The provider catalog comes from Rig's registry, so SDK additions need no BONE switch.
pub fn providers() -> Vec<String> {
    let mut providers: Vec<String> = ProviderId::all().map(|id| id.to_string()).collect();
    providers.extend(["cohere".into(), "ollama".into()]);
    #[cfg(feature = "bedrock")]
    providers.push("bedrock".into());
    #[cfg(feature = "vertexai")]
    providers.push("vertexai".into());
    #[cfg(feature = "candle")]
    providers.push("candle".into());
    providers
}

#[cfg(feature = "bedrock")]
fn bedrock_model(model: &str) -> Result<DynModel<Completion>> {
    Ok(rig_bedrock::client::BedrockRuntime::from_env()
        .completion(model)
        .erase())
}
#[cfg(not(feature = "bedrock"))]
fn bedrock_model(_: &str) -> Result<DynModel<Completion>> {
    bail!("Bedrock requires building BONE with --features bedrock")
}
#[cfg(feature = "vertexai")]
fn vertexai_model(model: &str) -> Result<DynModel<Completion>> {
    Ok(rig_vertexai::VertexAi::from_env()?
        .completion(model)
        .erase())
}
#[cfg(not(feature = "vertexai"))]
fn vertexai_model(_: &str) -> Result<DynModel<Completion>> {
    bail!("Vertex AI requires building BONE with --features vertexai")
}
#[cfg(feature = "candle")]
async fn candle_model(artifacts: &crate::config::CandleArtifacts) -> Result<DynModel<Completion>> {
    let data = rig_candle::ModelData {
        config: tokio::fs::read(&artifacts.config).await?,
        tokenizer: tokio::fs::read(&artifacts.tokenizer).await?,
        weights: tokio::fs::read(&artifacts.weights).await?,
    };
    let model = if artifacts.gguf {
        rig_candle::CandleModel::builder_from_artifacts(rig_candle::ModelArtifacts::Gguf(data))
            .build_async()
            .await?
    } else {
        rig_candle::CandleModel::builder(data).build_async().await?
    };
    Ok(model.completion().erase())
}
#[cfg(not(feature = "candle"))]
async fn candle_model(_: &crate::config::CandleArtifacts) -> Result<DynModel<Completion>> {
    bail!("Candle requires building BONE with --features candle")
}

#[cfg(test)]
#[path = "../tests/unit/model.rs"]
mod tests;
