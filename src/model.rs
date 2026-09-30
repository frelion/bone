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
        registry::{Provider, ProviderConfig, ProviderId},
    },
};
use sha2::{Digest, Sha256};

use crate::config::{ModelReference, Profile, validate_profile_name};

/// Keep this owner alive until the native call or stream is completely finished.
/// The private guard serializes subscription refresh AND inference across processes.
pub struct ModelConnection {
    pub model: DynModel<Completion>,
    pub job_id: String,
    pub call_id: String,
    _credential_lock: Option<CredentialLock>,
}

struct CredentialLock(File);

impl Drop for CredentialLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

pub async fn connect(
    profile: &Profile,
    data_dir: &Path,
    profile_name: &str,
    job_id: &str,
    call_id: &str,
) -> Result<ModelConnection> {
    connect_with(
        profile,
        data_dir,
        profile_name,
        job_id,
        call_id,
        DynHttpClient::new(rig_reqwest::shared()),
    )
    .await
}

/// Transport injection is native Rig HTTP; no BONE provider protocol sits in between.
pub async fn connect_with(
    profile: &Profile,
    data_dir: &Path,
    profile_name: &str,
    job_id: &str,
    call_id: &str,
    http: DynHttpClient,
) -> Result<ModelConnection> {
    ensure!(
        !job_id.trim().is_empty() && !call_id.trim().is_empty(),
        "every model invocation requires a JobId and CallId"
    );
    validate_profile_name(profile_name)?;
    profile.validate()?;
    let mut credential_lock = None;
    let model = match &profile.model {
        ModelReference::Registry(reference) if profile.is_subscription() => {
            let (guard, auth_file, source) = if profile.reuse_codex_login {
                let (guard, canonical_source) = lock_codex_source(
                    &codex_auth_file()?,
                    &crate::config::default_data_dir().join("credential-locks"),
                )
                .await?;
                (guard, None, load_codex_login(&canonical_source)?)
            } else {
                let (guard, auth_file) = lock_profile(data_dir, profile_name).await?;
                (guard, Some(auth_file), AuthSource::OAuth)
            };
            let ProviderConfig::OpenAi(config) = reference.config("") else {
                bail!("ChatGPT requires its native OpenAI dialect")
            };
            let auth = Authenticator::new(source, auth_file, DeviceCodeHandler::new(|_| {}), false);
            let client = config
                .connect(http)
                .authenticate(&auth)
                .await
                .map_err(|error| authentication_error(profile_name, error))?;
            if !profile.reuse_codex_login {
                restrict_cache_permissions(&self::auth_file(data_dir, profile_name)?)?;
            }
            credential_lock = Some(guard);
            client.completion(reference.model()).erase()
        }
        ModelReference::Registry(reference) => {
            if let Some(name) = &profile.credential_env {
                reference.completion_model_with(
                    read_credential(
                        name,
                        reference.id().is_none_or(|id| id.requires_credential()),
                    )?,
                    http,
                )
            } else {
                // Let Rig read every native endpoint and alternate-auth variable for
                // registered selections. Explicit recipes preserve their own endpoint.
                let config = match reference.provider() {
                    Provider::Registered(id) => native_env_config(*id)?,
                    Provider::Configured(config) => {
                        let id = config.id().context("configured provider has no registered credential source; set credential_env")?;
                        let native = native_env_config(id)?;
                        let mut configured = config.clone();
                        if let (ProviderConfig::OpenAi(recipe), ProviderConfig::OpenAi(resolved)) =
                            (&mut configured, &native)
                            && resolved.auth != recipe.dialect.quirks.auth
                        {
                            recipe.auth = resolved.auth;
                        }
                        let secret = match native {
                            ProviderConfig::OpenAi(c) => c.api_key,
                            ProviderConfig::Anthropic(c) => c.api_key,
                            ProviderConfig::Gemini(c) => c.api_key,
                        };
                        configured.with_credential(secret)
                    }
                };
                configured_model(config, reference.model(), http)
            }
        }
        ModelReference::Cohere { cohere, model } => {
            let mut config = cohere.clone();
            config.api_key = read_credential(
                profile
                    .credential_env
                    .as_deref()
                    .unwrap_or("COHERE_API_KEY"),
                true,
            )?
            .into();
            config.connect(http).completion(model).erase()
        }
        ModelReference::Ollama { ollama, model } => {
            let mut config = ollama.clone();
            config.api_key = read_credential(
                profile
                    .credential_env
                    .as_deref()
                    .unwrap_or("OLLAMA_API_KEY"),
                false,
            )?
            .into();
            config.connect(http).completion(model).erase()
        }
        ModelReference::Bedrock { bedrock } => bedrock_model(bedrock)?,
        ModelReference::VertexAi { vertexai } => vertexai_model(vertexai)?,
        ModelReference::Candle { candle } => candle_model(candle).await?,
    };
    Ok(ModelConnection {
        model,
        job_id: job_id.into(),
        call_id: call_id.into(),
        _credential_lock: credential_lock,
    })
}

fn configured_model(
    config: ProviderConfig,
    model: &str,
    http: DynHttpClient,
) -> DynModel<Completion> {
    match config {
        ProviderConfig::OpenAi(config) => config.connect(http).completion(model).erase(),
        ProviderConfig::Anthropic(config) => config.connect(http).completion(model).erase(),
        ProviderConfig::Gemini(config) => config.connect(http).completion(model).erase(),
    }
}

fn native_env_config(id: ProviderId) -> Result<ProviderConfig> {
    use rig_core::providers::{
        anthropic::AnthropicConfig, gemini::GeminiConfig, openai::OpenAIConfig,
    };
    let recipe = id.config("");
    Ok(match recipe {
        ProviderConfig::OpenAi(config) => ProviderConfig::OpenAi(
            OpenAIConfig::from_env_with(&config.dialect).map_err(environment_error)?,
        ),
        ProviderConfig::Anthropic(config) => ProviderConfig::Anthropic(
            AnthropicConfig::from_env_with(&config.dialect).map_err(environment_error)?,
        ),
        ProviderConfig::Gemini(_) => {
            ProviderConfig::Gemini(GeminiConfig::from_env().map_err(environment_error)?)
        }
    })
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
        PathBuf::from(std::env::var_os("HOME").context("cannot find Codex login directory")?)
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
    let guard = acquire_credential_lock(&lock_directory.join(format!("{identity}.lock"))).await?;
    Ok((guard, canonical_source))
}

async fn acquire_credential_lock(path: &Path) -> Result<CredentialLock> {
    let directory = path.parent().context("subscription lock parent missing")?;
    std::fs::create_dir_all(directory).context("cannot create BONE subscription lock directory")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
    }
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
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
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
    anyhow::anyhow!("{reason}; run `bone login --profile {profile_name}` to sign in again")
}

pub fn call_error(profile_name: &str, error: ProviderError) -> anyhow::Error {
    if error.report().http_status == Some(401) {
        anyhow::anyhow!(
            "provider rejected authentication (401); run `bone login --profile {profile_name}` for subscription profiles or update the configured credential"
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
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancelling_a_profile_lock_waiter_leaves_no_hidden_lease() {
        let directory = tempfile::tempdir().unwrap();
        let (owner, _) = lock_profile(directory.path(), "fixture").await.unwrap();
        let data = directory.path().to_owned();
        let waiter = tokio::spawn(async move { lock_profile(&data, "fixture").await });
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
        assert!(!waiter.is_finished());
        waiter.abort();
        assert!(matches!(waiter.await, Err(error) if error.is_cancelled()));
        drop(owner);
        let (next, _) = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            lock_profile(directory.path(), "fixture"),
        )
        .await
        .unwrap()
        .unwrap();
        drop(next);
    }

    #[tokio::test]
    async fn reused_codex_source_serializes_across_distinct_profile_caches() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("codex-auth.json");
        std::fs::write(&source, "{}").unwrap();
        let locks = directory.path().join("shared-locks");
        let cache_a = directory.path().join("data-a");
        let cache_b = directory.path().join("data-b");
        // Independent BONE cache owners may exist, but the reused source's
        // physical lease is shared regardless of those data/profile identities.
        let (profile_a, _) = lock_profile(&cache_a, "alpha").await.unwrap();
        let (profile_b, _) = lock_profile(&cache_b, "beta").await.unwrap();
        let (source_owner, resolved) = lock_codex_source(&source, &locks).await.unwrap();
        let source_again = resolved.clone();
        let locks_again = locks.clone();
        let waiter =
            tokio::spawn(async move { lock_codex_source(&source_again, &locks_again).await });
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
        assert!(!waiter.is_finished());
        drop(profile_a);
        drop(profile_b);
        assert!(!waiter.is_finished());
        drop(source_owner);
        let (next, _) = tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        drop(next);
        assert!(std::fs::read_to_string(source).unwrap() == "{}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn codex_source_symlink_alias_shares_lock_but_distinct_sources_do_not() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.json");
        let alias = directory.path().join("alias.json");
        let other = directory.path().join("other.json");
        std::fs::write(&source, "{}").unwrap();
        std::fs::write(&other, "{}").unwrap();
        std::os::unix::fs::symlink(&source, &alias).unwrap();
        let locks = directory.path().join("locks");
        let (owner, _) = lock_codex_source(&source, &locks).await.unwrap();
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(60),
                lock_codex_source(&alias, &locks)
            )
            .await
            .is_err()
        );
        let (independent, _) = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            lock_codex_source(&other, &locks),
        )
        .await
        .unwrap()
        .unwrap();
        drop(independent);
        drop(owner);
        let (next, _) = lock_codex_source(&alias, &locks).await.unwrap();
        drop(next);
    }

    #[test]
    fn reuse_codex_login_reads_current_access_token_without_modifying_source() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("auth.json");
        let first = r#"{"auth_mode":"chatgpt","tokens":{"access_token":"synthetic-first","account_id":"fixture-account","refresh_token":"never-used","id_token":"never-used"}}"#;
        std::fs::write(&path, first).unwrap();
        let AuthSource::AccessToken {
            access_token,
            account_id,
        } = load_codex_login(&path).unwrap()
        else {
            panic!("native AccessToken source required")
        };
        assert!(access_token == "synthetic-first");
        assert!(account_id.as_deref() == Some("fixture-account"));
        assert!(std::fs::read_to_string(&path).unwrap() == first);
        std::fs::write(
            &path,
            r#"{"tokens":{"access_token":"synthetic-current","account_id":"fixture-account"}}"#,
        )
        .unwrap();
        let AuthSource::AccessToken { access_token, .. } = load_codex_login(&path).unwrap() else {
            panic!("native AccessToken source required")
        };
        assert!(access_token == "synthetic-current");
    }

    #[test]
    fn invalid_codex_login_errors_never_echo_credential_values() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("auth.json");
        std::fs::write(&path, "synthetic-sensitive-broken-data").unwrap();
        let message = format!("{:#}", load_codex_login(&path).unwrap_err());
        assert!(message.contains("Codex login"));
        assert!(!message.contains("synthetic-sensitive-broken-data"));
    }

    fn cached_subscription(data_dir: &Path) -> Profile {
        let path = auth_file(data_dir, "test").unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // Entirely synthetic fixture authored by this test. No host cache is read.
        std::fs::write(
            path,
            r#"{"access_token":"bone-test-token","expires_at":4102444800}"#,
        )
        .unwrap();
        Profile::from_model("chatgpt:gpt-5.4").unwrap()
    }

    #[tokio::test]
    async fn cached_subscription_guard_survives_native_stream_creation() {
        let directory = tempfile::tempdir().unwrap();
        let profile = cached_subscription(directory.path());
        let connection = connect(&profile, directory.path(), "test", "job-1", "call-1")
            .await
            .unwrap();
        assert_eq!(connection.model.name(), "chatgpt");
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(directory.path().join("profiles/test/auth.lock"))
            .unwrap();
        assert!(lock.try_lock_exclusive().is_err());
        // Native streaming is lazy; its owner retains the lock while runtime
        // holds the stream. No live provider or real credentials are involved.
        let stream = connection
            .model
            .stream(rig_core::completion::CompletionRequest::new("fixture"))
            .unwrap();
        assert!(lock.try_lock_exclusive().is_err());
        drop(stream);
        drop(connection);
        lock.try_lock_exclusive().unwrap();
        FileExt::unlock(&lock).unwrap();
    }

    #[tokio::test]
    async fn invalid_oauth_cache_reports_login_without_cache_contents() {
        let directory = tempfile::tempdir().unwrap();
        let profile = cached_subscription(directory.path());
        std::fs::write(
            auth_file(directory.path(), "test").unwrap(),
            "synthetic-sensitive-invalid-cache",
        )
        .unwrap();
        let error = match connect(&profile, directory.path(), "test", "job", "call").await {
            Ok(_) => panic!("invalid cache accepted"),
            Err(error) => error,
        };
        let message = format!("{error:#}");
        assert!(message.contains("bone login --profile test"));
        assert!(!message.contains("synthetic-sensitive-invalid-cache"));
    }

    #[tokio::test]
    async fn missing_oauth_cache_never_starts_device_flow() {
        let directory = tempfile::tempdir().unwrap();
        let profile = Profile::from_model("chatgpt:gpt-5.4").unwrap();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            connect(&profile, directory.path(), "test", "job", "call"),
        )
        .await
        .unwrap();
        assert!(result.is_err());
        assert!(!auth_file(directory.path(), "test").unwrap().exists());
    }

    #[tokio::test]
    async fn model_connections_require_persistable_invocation_ids() {
        let directory = tempfile::tempdir().unwrap();
        let profile = Profile::from_model("ollama:qwen3").unwrap();
        assert!(
            connect(&profile, directory.path(), "test", "", "call")
                .await
                .is_err()
        );
        assert!(
            connect(&profile, directory.path(), "test", "job", " ")
                .await
                .is_err()
        );
    }
}
