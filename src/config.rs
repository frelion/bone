//! Persistent provider recipes. Rig owns provider configuration and its serialization.
use std::{
    collections::BTreeMap,
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use fs2::FileExt;
use rig_core::{
    completion::CompletionRequest,
    providers::{
        cohere::CohereConfig,
        ollama::OllamaConfig,
        registry::{Provider, ProviderConfig, ProviderRef},
    },
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Registry recipes remain native Rig references; companion providers retain their
/// native configuration too. These are connection recipes, never model responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
#[allow(
    clippy::large_enum_variant,
    reason = "The few persistent profiles retain native Rig recipes directly; these are not queued model events"
)]
pub enum ModelReference {
    Registry(ProviderRef),
    Cohere { cohere: CohereConfig, model: String },
    Ollama { ollama: OllamaConfig, model: String },
    Bedrock { bedrock: String },
    VertexAi { vertexai: String },
    Candle { candle: CandleArtifacts },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandleArtifacts {
    pub config: PathBuf,
    pub tokenizer: PathBuf,
    pub weights: PathBuf,
    #[serde(default)]
    pub gguf: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub model: ModelReference,
    /// The variable name only. Credential values belong to the environment or
    /// Rig's OAuth cache, never the profile document.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_env: Option<String>,
    /// Read the current Codex subscription token without changing its login file.
    #[serde(default)]
    pub reuse_codex_login: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_params: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
}

impl Profile {
    pub(crate) fn model_name(&self) -> Option<&str> {
        match &self.model {
            ModelReference::Registry(reference) => Some(reference.model()),
            ModelReference::Cohere { model, .. } | ModelReference::Ollama { model, .. } => {
                Some(model)
            }
            ModelReference::Bedrock { bedrock } => Some(bedrock),
            ModelReference::VertexAi { vertexai } => Some(vertexai),
            ModelReference::Candle { .. } => None,
        }
    }

    /// Native provider identity, independent of model and endpoint. Credentials
    /// stored by BONE are bound to this identity and never move across providers.
    pub fn provider_identity(&self) -> String {
        match &self.model {
            ModelReference::Registry(reference) => {
                let config = reference.config("");
                format!("{}/{}", config.vendor(), config.format())
            }
            ModelReference::Cohere { .. } => "cohere".into(),
            ModelReference::Ollama { .. } => "ollama".into(),
            ModelReference::Bedrock { .. } => "bedrock".into(),
            ModelReference::VertexAi { .. } => "vertexai".into(),
            ModelReference::Candle { .. } => "candle".into(),
        }
    }

    pub fn with_model(&self, value: &str) -> Result<Self> {
        let mut next = Self::from_model(value)?;
        if self.provider_identity() == next.provider_identity() {
            let model = match (&self.model, &next.model) {
                (ModelReference::Registry(old), ModelReference::Registry(new)) => {
                    ModelReference::Registry(match old.provider() {
                        Provider::Registered(id) => ProviderRef::registered(*id, new.model())?,
                        Provider::Configured(config) => {
                            ProviderRef::configured(config.clone(), new.model())?
                        }
                    })
                }
                (ModelReference::Cohere { cohere, .. }, ModelReference::Cohere { model, .. }) => {
                    ModelReference::Cohere {
                        cohere: cohere.clone(),
                        model: model.clone(),
                    }
                }
                (ModelReference::Ollama { ollama, .. }, ModelReference::Ollama { model, .. }) => {
                    ModelReference::Ollama {
                        ollama: ollama.clone(),
                        model: model.clone(),
                    }
                }
                _ => next.model.clone(),
            };
            next = self.clone();
            next.model = model;
        }
        next.validate()?;
        Ok(next)
    }

    pub fn endpoint(&self) -> Option<String> {
        match &self.model {
            ModelReference::Registry(reference) => Some(match reference.config("") {
                ProviderConfig::OpenAi(config) => config.base_url,
                ProviderConfig::Anthropic(config) => config.base_url,
                ProviderConfig::Gemini(config) => config.base_url,
            }),
            ModelReference::Cohere { cohere, .. } => Some(cohere.base_url.clone()),
            ModelReference::Ollama { ollama, .. } => Some(ollama.base_url.clone()),
            _ => None,
        }
    }

    /// Change the host on the native recipe; dialect, route and options stay native.
    pub fn with_endpoint(&self, endpoint: &str) -> Result<Self> {
        let uri: rig_core::http_client::Uri = endpoint
            .parse()
            .context("endpoint must be an absolute HTTP or HTTPS URL")?;
        ensure!(
            matches!(uri.scheme_str(), Some("http" | "https")) && uri.authority().is_some(),
            "endpoint must be an absolute HTTP or HTTPS URL"
        );
        let mut next = self.clone();
        match &mut next.model {
            ModelReference::Registry(reference) => {
                let mut config = reference.config("");
                match &mut config {
                    ProviderConfig::OpenAi(config) => config.base_url = endpoint.into(),
                    ProviderConfig::Anthropic(config) => config.base_url = endpoint.into(),
                    ProviderConfig::Gemini(config) => config.base_url = endpoint.into(),
                }
                *reference = ProviderRef::configured(config, reference.model())?;
            }
            ModelReference::Cohere { cohere, .. } => cohere.base_url = endpoint.into(),
            ModelReference::Ollama { ollama, .. } => ollama.base_url = endpoint.into(),
            _ => bail!("this native provider does not use an API endpoint recipe"),
        }
        Ok(next)
    }

    pub fn from_model(reference: &str) -> Result<Self> {
        let model = match reference.split_once(':') {
            Some(("cohere", model)) if !model.is_empty() => ModelReference::Cohere {
                cohere: CohereConfig::new(""),
                model: model.into(),
            },
            Some(("ollama", model)) if !model.is_empty() => ModelReference::Ollama {
                ollama: OllamaConfig::new(),
                model: model.into(),
            },
            Some(("bedrock", model)) if !model.is_empty() => ModelReference::Bedrock {
                bedrock: model.into(),
            },
            Some(("vertexai", model)) if !model.is_empty() => ModelReference::VertexAi {
                vertexai: model.into(),
            },
            _ => ModelReference::Registry(ProviderRef::parse(reference)?),
        };
        Ok(Self {
            model,
            credential_env: None,
            reuse_codex_login: false,
            additional_params: None,
            max_tokens: None,
        })
    }

    pub fn apply(&self, mut request: CompletionRequest) -> CompletionRequest {
        if let Some(params) = &self.additional_params {
            request = request.additional_params(params.clone());
        }
        if let Some(max_tokens) = self.max_tokens {
            request = request.max_tokens(max_tokens);
        }
        request
    }

    pub fn is_subscription(&self) -> bool {
        matches!(&self.model, ModelReference::Registry(reference) if reference.id().is_some_and(|id| id.vendor() == "chatgpt"))
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.reuse_codex_login || self.is_subscription(),
            "reuse_codex_login requires a ChatGPT subscription profile"
        );
        if let Some(name) = &self.credential_env {
            ensure!(
                !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
                "credential_env must be an environment variable name"
            );
        }
        if let Some(params) = &self.additional_params {
            ensure!(
                params.is_object(),
                "additional_params must be a JSON object"
            );
        }
        ensure!(self.max_tokens != Some(0), "max_tokens must be positive");
        let model = match &self.model {
            ModelReference::Registry(_) | ModelReference::Candle { .. } => None,
            ModelReference::Cohere { model, .. } | ModelReference::Ollama { model, .. } => {
                Some(model)
            }
            ModelReference::Bedrock { bedrock } => Some(bedrock),
            ModelReference::VertexAi { vertexai } => Some(vertexai),
        };
        ensure!(
            model.is_none_or(|m| !m.trim().is_empty()),
            "model identifier must not be empty"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub default_profile: String,
    pub profiles: BTreeMap<String, Profile>,
}

/// Exact on-disk revision, including absence. Never contains credential values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigRevision(Option<[u8; 32]>);

impl ConfigRevision {
    fn read(path: &Path) -> Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => Ok(Self(Some(Sha256::digest(bytes).into()))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self(None)),
            Err(error) => Err(error).context("cannot read current configuration revision"),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            default_profile: "default".into(),
            profiles: BTreeMap::from([(
                "default".into(),
                Profile::from_model("openai:gpt-5.4").expect("fixed registered default"),
            )]),
        }
    }
}

impl Config {
    pub fn load(data_dir: &Path) -> Result<Self> {
        Self::load_with_revision(data_dir).map(|(config, _)| config)
    }

    pub fn load_with_revision(data_dir: &Path) -> Result<(Self, ConfigRevision)> {
        let path = data_dir.join("config.toml");
        let (config, revision) = match std::fs::read(&path) {
            Ok(bytes) => {
                let revision = ConfigRevision(Some(Sha256::digest(&bytes).into()));
                let text =
                    std::str::from_utf8(&bytes).context("provider configuration is not UTF-8")?;
                (
                    toml::from_str::<Self>(text).with_context(|| {
                        format!("invalid provider configuration in {}", path.display())
                    })?,
                    revision,
                )
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                (Self::default(), ConfigRevision(None))
            }
            Err(error) => {
                return Err(error).with_context(|| format!("cannot read {}", path.display()));
            }
        };
        config.validate()?;
        Ok((config, revision))
    }

    /// Compare the exact loaded revision before committing. Other BONE writers
    /// share the lock; external edits observed before rename reject the save.
    pub fn save_checked(
        &self,
        data_dir: &Path,
        expected: &ConfigRevision,
    ) -> Result<ConfigRevision> {
        self.validate()?;
        std::fs::create_dir_all(data_dir).context("cannot create BONE configuration directory")?;
        let mut options = OpenOptions::new();
        options.create(true).truncate(false).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options.open(data_dir.join("config.lock"))?;
        lock.try_lock_exclusive()
            .context("configuration is being saved by another process; retry")?;
        let path = data_dir.join("config.toml");
        ensure!(
            ConfigRevision::read(&path)? == *expected,
            "configuration changed outside this editor; reopen connections before saving"
        );
        let text = toml::to_string_pretty(self)?;
        write_atomic(&path, text.as_bytes(), Some(expected))?;
        Ok(ConfigRevision(Some(Sha256::digest(text.as_bytes()).into())))
    }

    pub fn profile(&self, name: Option<&str>) -> Result<&Profile> {
        let name = name.unwrap_or(&self.default_profile);
        validate_profile_name(name)?;
        self.profiles
            .get(name)
            .with_context(|| format!("provider profile `{name}` does not exist"))
    }

    pub fn validate(&self) -> Result<()> {
        validate_profile_name(&self.default_profile)?;
        if !self.profiles.contains_key(&self.default_profile) {
            bail!("default_profile does not name a configured profile");
        }
        for (name, profile) in &self.profiles {
            validate_profile_name(name)?;
            profile
                .validate()
                .with_context(|| format!("invalid profile `{name}`"))?;
        }
        Ok(())
    }
}

pub(crate) fn write_atomic(
    path: &Path,
    bytes: &[u8],
    expected: Option<&ConfigRevision>,
) -> Result<()> {
    let parent = path.parent().context("saved file parent is missing")?;
    std::fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".bone-save-{}", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        crate::filesystem::private_file(&file)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        if let Some(expected) = expected {
            ensure!(
                ConfigRevision::read(path)? == *expected,
                "configuration changed outside this editor; reopen connections before saving"
            );
        }
        drop(file);
        crate::filesystem::replace(&temporary, path)?;
        crate::filesystem::sync_directory(parent)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

pub fn validate_profile_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= 80
            && name == name.trim()
            && name
                .chars()
                .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '_' | '-')),
        "connection names support Unicode letters, digits, spaces, underscores or hyphens; no leading or trailing spaces, at most 80 UTF-8 bytes"
    );
    Ok(())
}

pub fn default_data_dir() -> PathBuf {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".bone/v2")
}

#[cfg(unix)]
pub(crate) fn user_home() -> Result<PathBuf> {
    use std::ffi::CStr;
    use std::os::unix::ffi::OsStrExt;
    let mut buffer = vec![0_u8; 64 * 1024];
    let mut record = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut result = std::ptr::null_mut();
    // getpwuid_r returns a pointer into our buffer, used before it is dropped.
    let error = unsafe {
        libc::getpwuid_r(
            libc::geteuid(),
            record.as_mut_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut result,
        )
    };
    ensure!(
        error == 0 && !result.is_null(),
        "cannot find stable home directory for current user"
    );
    let record = unsafe { record.assume_init() };
    ensure!(!record.pw_dir.is_null(), "user home directory is missing");
    let home = unsafe { CStr::from_ptr(record.pw_dir) };
    Ok(PathBuf::from(std::ffi::OsStr::from_bytes(home.to_bytes())))
}

#[cfg(windows)]
pub(crate) fn user_home() -> Result<PathBuf> {
    crate::windows::user_home().context("cannot find stable OS user profile directory")
}

#[cfg(test)]
#[path = "../tests/unit/config.rs"]
mod tests;
