//! Persistent provider recipes. Rig owns provider configuration and its serialization.
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use rig_core::{
    completion::CompletionRequest,
    providers::{cohere::CohereConfig, ollama::OllamaConfig, registry::ProviderRef},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

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
        let path = data_dir.join("config.toml");
        let config = match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str::<Self>(&text)
                .with_context(|| format!("invalid provider configuration in {}", path.display()))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(error) => {
                return Err(error).with_context(|| format!("cannot read {}", path.display()));
            }
        };
        config.validate()?;
        Ok(config)
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

pub fn validate_profile_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= 80
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
        "profile names must contain 1–80 letters, digits, underscores or hyphens"
    );
    Ok(())
}

pub fn default_data_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".bone/v2")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_core::providers::{
        openai::{OpenAIConfig, Route},
        registry::ProviderConfig,
    };

    #[test]
    fn native_config_roundtrip_preserves_route_endpoint_and_omits_key() {
        let config = ProviderConfig::OpenAi(
            OpenAIConfig::new("test-secret")
                .with_base_url("http://127.0.0.1:1234/v1")
                .with_route(Route::Responses),
        );
        let profile = Profile {
            model: ModelReference::Registry(ProviderRef::configured(config, "fixture").unwrap()),
            credential_env: Some("BONE_FIXTURE_KEY".into()),
            reuse_codex_login: false,
            additional_params: Some(serde_json::json!({"reasoning":{"effort":"low"}})),
            max_tokens: Some(100),
        };
        let config = Config {
            default_profile: "fixture".into(),
            profiles: BTreeMap::from([("fixture".into(), profile)]),
        };
        let text = toml::to_string(&config).unwrap();
        assert!(!text.contains("test-secret"));
        let restored: Config = toml::from_str(&text).unwrap();
        restored.validate().unwrap();
        let ModelReference::Registry(reference) = &restored.profile(None).unwrap().model else {
            panic!("registry recipe")
        };
        let ProviderConfig::OpenAi(native) = reference.config("") else {
            panic!("OpenAI config")
        };
        assert_eq!(native.base_url, "http://127.0.0.1:1234/v1");
        assert_eq!(native.route, Some(Route::Responses));
    }

    #[test]
    fn unsafe_profile_names_and_empty_companion_models_are_rejected() {
        for name in ["", "..", "../other", "a/b", "a\\b"] {
            assert!(validate_profile_name(name).is_err());
        }
        let profile = Profile {
            model: ModelReference::Bedrock { bedrock: "".into() },
            credential_env: None,
            reuse_codex_login: false,
            additional_params: None,
            max_tokens: None,
        };
        assert!(profile.validate().is_err());
    }
}
