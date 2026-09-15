use std::path::PathBuf;

use bone_adapters::{
    llm::{EndpointConfig, ModelOptions, protocol::openai_responses::ReasoningEffort},
    tools::ToolLimits,
};
use bone_core::AgentLimits;
use serde::{Deserialize, Serialize};

use crate::{SessionId, WorkspaceId};

const CHATGPT_PROFILE: &str = "chatgpt";
pub(crate) const MAX_PERSISTED_VALUE_BYTES: usize = 1024 * 1024;
// Bash retains stdout and stderr separately; JSON can escape each byte as six bytes.
const MAX_TOOL_STREAM_BYTES: usize = 512 * 1024;

/// A curated model choice for a first-party connection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ModelPreset {
    pub id: &'static str,
    pub label: &'static str,
    pub note: &'static str,
    pub recommended: bool,
    pub default_reasoning: Option<ReasoningEffort>,
    pub supported_reasoning: &'static [ReasoningEffort],
}

const NONE_TO_MAX: &[ReasoningEffort] = &[
    ReasoningEffort::None,
    ReasoningEffort::Low,
    ReasoningEffort::Medium,
    ReasoningEffort::High,
    ReasoningEffort::Xhigh,
    ReasoningEffort::Max,
];
const LOW_TO_MAX: &[ReasoningEffort] = &[
    ReasoningEffort::Low,
    ReasoningEffort::Medium,
    ReasoningEffort::High,
    ReasoningEffort::Xhigh,
    ReasoningEffort::Max,
];
const NONE_TO_XHIGH: &[ReasoningEffort] = &[
    ReasoningEffort::None,
    ReasoningEffort::Low,
    ReasoningEffort::Medium,
    ReasoningEffort::High,
    ReasoningEffort::Xhigh,
];
const LOW_TO_XHIGH: &[ReasoningEffort] = &[
    ReasoningEffort::Low,
    ReasoningEffort::Medium,
    ReasoningEffort::High,
    ReasoningEffort::Xhigh,
];

// ChatGPT subscription access uses the Codex backend, so this picker follows
// the current Codex product catalog rather than the general ChatGPT model list.
const CHATGPT_MODEL_PRESETS: &[ModelPreset] = &[
    ModelPreset {
        id: "gpt-6-astra",
        label: "GPT-6 Astra",
        note: "Highest capability for difficult, long-running work",
        recommended: false,
        default_reasoning: Some(ReasoningEffort::Medium),
        supported_reasoning: LOW_TO_MAX,
    },
    ModelPreset {
        id: "gpt-5.6-sol",
        label: "GPT-5.6 Sol",
        note: "Strong professional model for demanding work",
        recommended: false,
        default_reasoning: Some(ReasoningEffort::Medium),
        supported_reasoning: NONE_TO_MAX,
    },
    ModelPreset {
        id: "gpt-5.6-terra",
        label: "GPT-5.6 Terra",
        note: "Balanced capability and speed for everyday coding",
        recommended: true,
        default_reasoning: Some(ReasoningEffort::Medium),
        supported_reasoning: NONE_TO_MAX,
    },
    ModelPreset {
        id: "gpt-5.6-luna",
        label: "GPT-5.6 Luna",
        note: "Fast, efficient model for routine work",
        recommended: false,
        default_reasoning: Some(ReasoningEffort::Medium),
        supported_reasoning: NONE_TO_MAX,
    },
    ModelPreset {
        id: "gpt-5.5",
        label: "GPT-5.5",
        note: "Reliable previous-generation model",
        recommended: false,
        default_reasoning: Some(ReasoningEffort::Medium),
        supported_reasoning: NONE_TO_XHIGH,
    },
    ModelPreset {
        id: "gpt-5.3-codex-spark",
        label: "GPT-5.3 Codex Spark",
        note: "Fast coding model for short, focused tasks",
        recommended: false,
        default_reasoning: Some(ReasoningEffort::Medium),
        supported_reasoning: LOW_TO_XHIGH,
    },
];

const OPENAI_MODEL_PRESETS: &[ModelPreset] = &[
    ModelPreset {
        id: "gpt-5.6-sol",
        label: "GPT-5.6 Sol",
        note: "Reliable model for demanding everyday work",
        recommended: true,
        default_reasoning: Some(ReasoningEffort::Medium),
        supported_reasoning: NONE_TO_MAX,
    },
    ModelPreset {
        id: "gpt-5.6-terra",
        label: "GPT-5.6 Terra",
        note: "Balanced model for everyday coding",
        recommended: false,
        default_reasoning: Some(ReasoningEffort::Medium),
        supported_reasoning: NONE_TO_MAX,
    },
    ModelPreset {
        id: "gpt-5.6-luna",
        label: "GPT-5.6 Luna",
        note: "Fast and affordable for routine work",
        recommended: false,
        default_reasoning: Some(ReasoningEffort::Medium),
        supported_reasoning: NONE_TO_MAX,
    },
    ModelPreset {
        id: "gpt-5.5",
        label: "GPT-5.5",
        note: "Proven previous-generation model",
        recommended: false,
        default_reasoning: Some(ReasoningEffort::Medium),
        supported_reasoning: NONE_TO_XHIGH,
    },
];

const ANTHROPIC_MODEL_PRESETS: &[ModelPreset] = &[
    ModelPreset {
        id: "claude-sonnet-4-6",
        label: "Claude Sonnet 4.6",
        note: "Balanced speed and capability",
        recommended: true,
        default_reasoning: None,
        supported_reasoning: &[],
    },
    ModelPreset {
        id: "claude-opus-4-8",
        label: "Claude Opus 4.8",
        note: "Most capable for complex work",
        recommended: false,
        default_reasoning: None,
        supported_reasoning: &[],
    },
    ModelPreset {
        id: "claude-haiku-4-5",
        label: "Claude Haiku 4.5",
        note: "Fastest and lowest-cost Claude model",
        recommended: false,
        default_reasoning: None,
        supported_reasoning: &[],
    },
];

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct ProfileId(String);

impl ProfileId {
    pub fn new(value: impl Into<String>) -> Result<Self, ConfigError> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
        if valid {
            Ok(Self(value))
        } else {
            Err(ConfigError::InvalidProfileId)
        }
    }

    pub fn chatgpt() -> Self {
        Self(CHATGPT_PROFILE.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ProfileId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ProfileId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub id: ProfileId,
    pub label: String,
    pub endpoint: EndpointConfig,
}

impl Profile {
    pub fn new(
        id: ProfileId,
        label: impl Into<String>,
        endpoint: EndpointConfig,
    ) -> Result<Self, ConfigError> {
        let profile = Self {
            id,
            label: label.into(),
            endpoint,
        };
        profile.validate()?;
        Ok(profile)
    }

    pub fn chatgpt() -> Self {
        Self {
            id: ProfileId::chatgpt(),
            label: "ChatGPT subscription".into(),
            endpoint: EndpointConfig::ChatGptSubscription,
        }
    }

    /// Returns the curated choices for an official provider connection.
    ///
    /// Compatible services use their own model IDs, so profiles with a custom
    /// base URL intentionally have no presets.
    pub fn model_presets(&self) -> &'static [ModelPreset] {
        match &self.endpoint {
            EndpointConfig::ChatGptSubscription => CHATGPT_MODEL_PRESETS,
            EndpointConfig::OpenAiResponses { base_url: None }
            | EndpointConfig::OpenAiChatCompletions { base_url: None } => OPENAI_MODEL_PRESETS,
            EndpointConfig::AnthropicMessages { base_url: None } => ANTHROPIC_MODEL_PRESETS,
            EndpointConfig::OpenAiResponses { base_url: Some(_) }
            | EndpointConfig::OpenAiChatCompletions { base_url: Some(_) }
            | EndpointConfig::AnthropicMessages { base_url: Some(_) } => &[],
        }
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.label.trim().is_empty() || self.label.trim() != self.label || self.label.len() > 128
        {
            return Err(ConfigError::InvalidProfileLabel);
        }
        self.endpoint
            .validate()
            .map_err(|_| ConfigError::InvalidEndpoint)?;
        if self.endpoint.base_url().is_some_and(|base_url| {
            base_url
                .split_once("://")
                .is_none_or(|(scheme, _)| !scheme.eq_ignore_ascii_case("https"))
        }) {
            return Err(ConfigError::InsecureEndpoint);
        }
        if matches!(self.endpoint, EndpointConfig::ChatGptSubscription)
            && self.id != ProfileId::chatgpt()
        {
            return Err(ConfigError::InvalidEndpoint);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelSelection {
    pub profile: ProfileId,
    pub model: String,
    pub options: Option<ModelOptions>,
}

impl ModelSelection {
    pub fn new(profile: ProfileId, model: impl Into<String>) -> Result<Self, ConfigError> {
        let selection = Self {
            profile,
            model: model.into(),
            options: None,
        };
        selection.validate()?;
        Ok(selection)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.model.trim().is_empty()
            || self.model.trim() != self.model
            || self.model.len() > 256
            || self.model.chars().any(char::is_control)
        {
            return Err(ConfigError::InvalidModel);
        }
        if let Some(options) = &self.options {
            options
                .validate()
                .map_err(|_| ConfigError::InvalidModelOptions)?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum ToolMode {
    #[default]
    ReadOnly,
    WorkspaceWrite,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolSettings {
    pub mode: ToolMode,
    pub limits: ToolLimits,
}

impl Default for ToolSettings {
    fn default() -> Self {
        Self {
            mode: ToolMode::ReadOnly,
            limits: ToolLimits::default(),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RuntimeSettings {
    pub worker: Option<ModelSelection>,
    pub coordinator: Option<ModelSelection>,
    pub limits: AgentLimits,
    pub tools: ToolSettings,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeOverrides {
    pub worker: Option<ModelSelection>,
    pub coordinator: Option<ModelSelection>,
    pub limits: Option<AgentLimits>,
    pub tools: Option<ToolSettings>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ConfigScope {
    User,
    Workspace(WorkspaceId),
    Session(SessionId),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ConfigChange {
    Model(Option<ModelSelection>),
    Worker(Option<ModelSelection>),
    Coordinator(Option<ModelSelection>),
    Limits(Option<AgentLimits>),
    Tools(Option<ToolSettings>),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ConfigProblem {
    NeedsModel,
    MissingProfile(ProfileId),
    Invalid(String),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ResolvedConfig {
    pub desired: Result<RuntimeConfig, ConfigProblem>,
    pub running: Option<RuntimeConfig>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ResolvedModel {
    pub selection: ModelSelection,
    pub profile: Profile,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimeConfig {
    pub coordinator: ResolvedModel,
    pub worker: ResolvedModel,
    pub limits: AgentLimits,
    pub tools: ToolSettings,
    pub workspace: PathBuf,
}

pub(crate) fn resolve_runtime(
    global: &RuntimeSettings,
    workspace: Option<&RuntimeOverrides>,
    session: Option<&RuntimeOverrides>,
    profiles: &[Profile],
    root: PathBuf,
) -> Result<RuntimeConfig, ConfigProblem> {
    let worker = nearest(
        session.and_then(|value| value.worker.as_ref()),
        workspace.and_then(|value| value.worker.as_ref()),
        global.worker.as_ref(),
    )
    .cloned()
    .ok_or(ConfigProblem::NeedsModel)?;
    let coordinator = nearest(
        session.and_then(|value| value.coordinator.as_ref()),
        workspace.and_then(|value| value.coordinator.as_ref()),
        global.coordinator.as_ref(),
    )
    .cloned()
    .unwrap_or_else(|| worker.clone());
    let limits = session
        .and_then(|value| value.limits.clone())
        .or_else(|| workspace.and_then(|value| value.limits.clone()))
        .unwrap_or_else(|| global.limits.clone());
    let tools = session
        .and_then(|value| value.tools.clone())
        .or_else(|| workspace.and_then(|value| value.tools.clone()))
        .unwrap_or_else(|| global.tools.clone());

    validate_runtime(&worker, &coordinator, &limits, &tools)
        .map_err(|error| ConfigProblem::Invalid(error.to_string()))?;
    Ok(RuntimeConfig {
        coordinator: resolve_model(coordinator, profiles)?,
        worker: resolve_model(worker, profiles)?,
        limits,
        tools,
        workspace: root,
    })
}

fn nearest<'a>(
    session: Option<&'a ModelSelection>,
    workspace: Option<&'a ModelSelection>,
    user: Option<&'a ModelSelection>,
) -> Option<&'a ModelSelection> {
    session.or(workspace).or(user)
}

pub(crate) fn resolve_model(
    selection: ModelSelection,
    profiles: &[Profile],
) -> Result<ResolvedModel, ConfigProblem> {
    let profile = profiles
        .iter()
        .find(|profile| profile.id == selection.profile)
        .cloned()
        .ok_or_else(|| ConfigProblem::MissingProfile(selection.profile.clone()))?;
    if let Some(options) = &selection.options {
        options
            .validate_for(&profile.endpoint)
            .map_err(|error| ConfigProblem::Invalid(error.to_string()))?;
    }
    Ok(ResolvedModel { selection, profile })
}

fn validate_runtime(
    worker: &ModelSelection,
    coordinator: &ModelSelection,
    limits: &AgentLimits,
    tools: &ToolSettings,
) -> Result<(), ConfigError> {
    worker.validate()?;
    coordinator.validate()?;
    validate_agent_limits(limits)?;
    validate_tool_settings(tools)?;
    if tools.mode == ToolMode::WorkspaceWrite
        && limits.tool_timeout <= tools.limits.max_bash_timeout
    {
        return Err(ConfigError::InvalidToolTimeout);
    }
    Ok(())
}

pub(crate) fn validate_agent_limits(limits: &AgentLimits) -> Result<(), ConfigError> {
    limits.validate().map_err(|_| ConfigError::InvalidLimits)?;
    if limits.context_bytes > MAX_PERSISTED_VALUE_BYTES
        || limits.item_bytes > MAX_PERSISTED_VALUE_BYTES
        || limits.tool_output_bytes > MAX_PERSISTED_VALUE_BYTES
    {
        return Err(ConfigError::PersistentPayloadLimit);
    }
    Ok(())
}

pub(crate) fn validate_tool_settings(tools: &ToolSettings) -> Result<(), ConfigError> {
    tools
        .limits
        .validate()
        .map_err(|_| ConfigError::InvalidLimits)?;
    if tools.limits.max_output_bytes > MAX_TOOL_STREAM_BYTES
        || tools.limits.max_patch_bytes > MAX_PERSISTED_VALUE_BYTES
    {
        return Err(ConfigError::PersistentPayloadLimit);
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ConfigError {
    #[error("invalid profile ID")]
    InvalidProfileId,
    #[error("invalid profile label")]
    InvalidProfileLabel,
    #[error("invalid endpoint")]
    InvalidEndpoint,
    #[error("compatible endpoint URLs must use HTTPS")]
    InsecureEndpoint,
    #[error("invalid model")]
    InvalidModel,
    #[error("invalid model options")]
    InvalidModelOptions,
    #[error("invalid runtime limits")]
    InvalidLimits,
    #[error("agent tool timeout must exceed the largest Bash timeout")]
    InvalidToolTimeout,
    #[error("runtime output or patch limits exceed App persistence capacity")]
    PersistentPayloadLimit,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_core::providers::{anthropic, openai};

    fn model(name: &str) -> ModelSelection {
        ModelSelection::new(ProfileId::new("test").unwrap(), name).unwrap()
    }

    fn profile() -> Profile {
        Profile::new(
            ProfileId::new("test").unwrap(),
            "Test",
            EndpointConfig::OpenAiResponses { base_url: None },
        )
        .unwrap()
    }

    fn profile_for(endpoint: EndpointConfig) -> Profile {
        Profile::new(ProfileId::new("test").unwrap(), "Test", endpoint).unwrap()
    }

    fn settings() -> RuntimeSettings {
        RuntimeSettings {
            worker: Some(model("user-worker")),
            coordinator: None,
            limits: AgentLimits::default(),
            tools: ToolSettings::default(),
        }
    }

    #[test]
    fn closest_scope_wins_and_coordinator_follows_worker() {
        let workspace = RuntimeOverrides {
            worker: Some(model("workspace-worker")),
            ..RuntimeOverrides::default()
        };
        let session = RuntimeOverrides {
            worker: Some(model("session-worker")),
            ..RuntimeOverrides::default()
        };
        let result = resolve_runtime(
            &settings(),
            Some(&workspace),
            Some(&session),
            &[profile()],
            PathBuf::from("/tmp/workspace"),
        )
        .unwrap();
        assert_eq!(result.worker.selection.model, "session-worker");
        assert_eq!(result.coordinator.selection.model, "session-worker");
    }

    #[test]
    fn missing_worker_is_a_normal_incomplete_configuration() {
        assert_eq!(
            resolve_runtime(
                &RuntimeSettings::default(),
                None,
                None,
                &[Profile::chatgpt()],
                PathBuf::from("/tmp/workspace"),
            ),
            Err(ConfigProblem::NeedsModel)
        );
    }

    #[test]
    fn profile_id_validation_cannot_be_bypassed_by_deserialization() {
        assert!(serde_json::from_str::<ProfileId>(r#""valid-id""#).is_ok());
        assert!(serde_json::from_str::<ProfileId>(r#""INVALID""#).is_err());
    }

    #[test]
    fn model_names_reject_terminal_control_characters() {
        let profile = ProfileId::new("test").unwrap();
        for name in ["gpt-5\nignore", "gpt-5\tignore", "gpt-5\u{1b}[31m"] {
            assert_eq!(
                ModelSelection::new(profile.clone(), name),
                Err(ConfigError::InvalidModel)
            );
        }
    }

    #[test]
    fn profiles_reject_plain_http_before_credentials_can_be_loaded() {
        let profile = Profile::new(
            ProfileId::new("insecure").unwrap(),
            "Insecure",
            EndpointConfig::OpenAiResponses {
                base_url: Some("http://127.0.0.1:8080/v1".into()),
            },
        );
        assert_eq!(profile, Err(ConfigError::InsecureEndpoint));
    }

    #[test]
    fn official_profiles_offer_curated_provider_models() {
        let chatgpt = Profile::chatgpt();
        assert_eq!(
            chatgpt
                .model_presets()
                .iter()
                .map(|preset| preset.id)
                .collect::<Vec<_>>(),
            [
                "gpt-6-astra",
                "gpt-5.6-sol",
                "gpt-5.6-terra",
                "gpt-5.6-luna",
                "gpt-5.5",
                "gpt-5.3-codex-spark",
            ]
        );
        assert!(chatgpt.model_presets().iter().all(|preset| {
            preset.default_reasoning == Some(ReasoningEffort::Medium)
                && preset
                    .supported_reasoning
                    .contains(&ReasoningEffort::Medium)
        }));

        let openai_models = [
            openai::GPT_5_6_SOL,
            openai::GPT_5_6_TERRA,
            openai::GPT_5_6_LUNA,
            openai::GPT_5_5,
        ];
        for endpoint in [
            EndpointConfig::OpenAiResponses { base_url: None },
            EndpointConfig::OpenAiChatCompletions { base_url: None },
        ] {
            assert_eq!(
                profile_for(endpoint)
                    .model_presets()
                    .iter()
                    .map(|preset| preset.id)
                    .collect::<Vec<_>>(),
                openai_models
            );
        }

        assert_eq!(
            profile_for(EndpointConfig::AnthropicMessages { base_url: None })
                .model_presets()
                .iter()
                .map(|preset| preset.id)
                .collect::<Vec<_>>(),
            [
                anthropic::completion::CLAUDE_SONNET_4_6,
                anthropic::completion::CLAUDE_OPUS_4_8,
                anthropic::completion::CLAUDE_HAIKU_4_5,
            ]
        );
    }

    #[test]
    fn every_official_catalog_has_one_clear_recommendation() {
        for presets in [
            Profile::chatgpt().model_presets(),
            profile_for(EndpointConfig::OpenAiResponses { base_url: None }).model_presets(),
            profile_for(EndpointConfig::AnthropicMessages { base_url: None }).model_presets(),
        ] {
            assert_eq!(
                presets.iter().filter(|preset| preset.recommended).count(),
                1
            );
            assert!(presets.iter().all(|preset| {
                !preset.id.is_empty() && !preset.label.is_empty() && !preset.note.is_empty()
            }));
        }
    }

    #[test]
    fn custom_endpoints_do_not_claim_provider_model_availability() {
        for endpoint in [
            EndpointConfig::OpenAiResponses {
                base_url: Some("https://example.com/responses".into()),
            },
            EndpointConfig::OpenAiChatCompletions {
                base_url: Some("https://example.com/chat".into()),
            },
            EndpointConfig::AnthropicMessages {
                base_url: Some("https://example.com/messages".into()),
            },
        ] {
            assert!(profile_for(endpoint).model_presets().is_empty());
        }
    }

    #[test]
    fn bash_stream_limits_leave_room_for_json_escaping() {
        let mut oversized_stream = settings();
        oversized_stream.tools.limits.max_output_bytes = MAX_TOOL_STREAM_BYTES + 1;
        let error = resolve_runtime(
            &oversized_stream,
            None,
            None,
            &[profile()],
            PathBuf::from("/tmp/workspace"),
        )
        .unwrap_err();
        assert!(matches!(error, ConfigProblem::Invalid(_)));

        let mut settings = settings();
        settings.limits.context_bytes = MAX_PERSISTED_VALUE_BYTES + 1;
        assert!(matches!(
            resolve_runtime(
                &settings,
                None,
                None,
                &[profile()],
                PathBuf::from("/tmp/workspace"),
            ),
            Err(ConfigProblem::Invalid(_))
        ));
    }
}
