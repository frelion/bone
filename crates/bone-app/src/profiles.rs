//! App-owned catalog of non-secret LLM connection profiles.
//!
//! A profile gives a user-facing name and stable identity to a
//! `bone_llm::EndpointConfig`.  The protocol configuration itself belongs to
//! `bone-llm`; this module only gives it an App lifecycle and a durable key.
//! API keys and OAuth state deliberately live elsewhere.

use std::fmt;

use bone_llm::{ConfigError, EndpointConfig};
use serde::{Deserialize, Deserializer, Serialize, de};
use thiserror::Error;

const MAX_PROFILE_ID_BYTES: usize = 64;
const MAX_PROFILE_LABEL_BYTES: usize = 128;
const RESERVED_PROFILE_IDS: &[&str] = &["default", "global", "coordinator", "inherit"];

/// The built-in profile used to migrate existing ChatGPT-only selections.
pub const CHATGPT_PROFILE_ID: &str = "chatgpt";

/// A stable, user-visible identifier for one saved LLM connection.
///
/// The format is deliberately also safe as the account name of an operating
/// system credential item.  It is not a provider name and has no secret
/// meaning.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct LlmProfileId(String);

impl LlmProfileId {
    pub fn new(value: impl Into<String>) -> Result<Self, LlmProfileIdError> {
        let value = value.into();
        if valid_profile_id(&value) {
            Ok(Self(value))
        } else {
            Err(LlmProfileIdError(value))
        }
    }

    pub fn chatgpt() -> Self {
        Self(CHATGPT_PROFILE_ID.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for LlmProfileId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for LlmProfileId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)
            .and_then(|value| Self::new(value).map_err(de::Error::custom))
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[error(
    "profile ID must be 1-{MAX_PROFILE_ID_BYTES} lowercase letters, digits, or single hyphens, and cannot be a reserved model scope"
)]
pub struct LlmProfileIdError(String);

/// One non-secret connection profile persisted by the App.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LlmProfile {
    pub id: LlmProfileId,
    pub label: String,
    pub endpoint: EndpointConfig,
}

impl LlmProfile {
    pub fn new(
        id: LlmProfileId,
        label: impl Into<String>,
        endpoint: EndpointConfig,
    ) -> Result<Self, LlmProfileError> {
        let profile = Self {
            id,
            label: label.into(),
            endpoint,
        };
        profile.validate()?;
        Ok(profile)
    }

    pub fn chatgpt_subscription() -> Self {
        Self {
            id: LlmProfileId::chatgpt(),
            label: "ChatGPT subscription".into(),
            endpoint: EndpointConfig::ChatGptSubscription,
        }
    }

    pub fn validate(&self) -> Result<(), LlmProfileError> {
        if self.label.trim().is_empty()
            || self.label.trim() != self.label
            || self.label.len() > MAX_PROFILE_LABEL_BYTES
        {
            return Err(LlmProfileError::InvalidLabel);
        }
        self.endpoint
            .validate()
            .map_err(LlmProfileError::Endpoint)?;
        if matches!(self.endpoint, EndpointConfig::ChatGptSubscription)
            && self.id.as_str() != CHATGPT_PROFILE_ID
        {
            return Err(LlmProfileError::ChatGptProfileId);
        }
        if let Some(base_url) = self.endpoint.base_url()
            && base_url
                .parse::<http::Uri>()
                .ok()
                .and_then(|uri| uri.scheme_str().map(str::to_owned))
                .as_deref()
                != Some("https")
        {
            return Err(LlmProfileError::InsecureBaseUrl);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum LlmProfileError {
    #[error(
        "profile label must be trimmed, non-empty, and at most {MAX_PROFILE_LABEL_BYTES} bytes"
    )]
    InvalidLabel,
    #[error(transparent)]
    Endpoint(#[from] ConfigError),
    #[error("compatible endpoint URLs must use HTTPS")]
    InsecureBaseUrl,
    #[error("the ChatGPT subscription is available only through the built-in `chatgpt` profile")]
    ChatGptProfileId,
}

/// The complete persisted profile catalog.  It is a distinct App document so
/// global appearance/tool/agent settings do not become a god configuration.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LlmProfiles {
    pub profiles: Vec<LlmProfile>,
}

impl Default for LlmProfiles {
    fn default() -> Self {
        Self {
            profiles: vec![LlmProfile::chatgpt_subscription()],
        }
    }
}

impl LlmProfiles {
    pub fn validate(&self) -> Result<(), LlmProfilesError> {
        for profile in &self.profiles {
            profile.validate()?;
        }
        for (index, profile) in self.profiles.iter().enumerate() {
            if self.profiles[..index]
                .iter()
                .any(|existing| existing.id == profile.id)
            {
                return Err(LlmProfilesError::DuplicateId(profile.id.clone()));
            }
        }
        Ok(())
    }

    pub fn get(&self, id: &LlmProfileId) -> Option<&LlmProfile> {
        self.profiles.iter().find(|profile| &profile.id == id)
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum LlmProfilesError {
    #[error(transparent)]
    Profile(#[from] LlmProfileError),
    #[error("a profile named `{0}` already exists")]
    DuplicateId(LlmProfileId),
}

fn valid_profile_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_PROFILE_ID_BYTES
        && !value.starts_with('-')
        && !value.ends_with('-')
        && !value.contains("--")
        && !RESERVED_PROFILE_IDS.contains(&value)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

#[cfg(test)]
mod tests {
    use bone_llm::EndpointConfig;

    use super::*;

    #[test]
    fn profile_ids_are_stable_credential_safe_names() {
        for value in ["openai", "work-gateway-2", "a1"] {
            assert!(LlmProfileId::new(value).is_ok(), "{value}");
        }
        for value in [
            "",
            "OpenAI",
            "has_space",
            "-edge",
            "edge-",
            "two--hyphens",
            "default",
            "global",
            "coordinator",
            "inherit",
        ] {
            assert!(LlmProfileId::new(value).is_err(), "{value}");
        }
    }

    #[test]
    fn persisted_profile_ids_are_validated_before_they_select_credentials() {
        let profile_id = serde_json::from_str::<LlmProfileId>("\"Not-Safe\"");
        assert!(profile_id.is_err());
    }

    #[test]
    fn compatible_profiles_require_https_before_an_api_key_can_be_sent() {
        let profile = LlmProfile::new(
            LlmProfileId::new("local").unwrap(),
            "Local",
            EndpointConfig::OpenAiResponses {
                base_url: Some("http://127.0.0.1:8080/v1".into()),
            },
        );
        assert_eq!(profile.unwrap_err(), LlmProfileError::InsecureBaseUrl);
    }

    #[test]
    fn app_has_one_unambiguous_chatgpt_subscription_profile() {
        let alias = LlmProfile::new(
            LlmProfileId::new("personal-chatgpt").unwrap(),
            "Personal ChatGPT",
            EndpointConfig::ChatGptSubscription,
        );
        assert_eq!(alias.unwrap_err(), LlmProfileError::ChatGptProfileId);
    }

    #[test]
    fn defaults_seed_the_legacy_chatgpt_profile_once() {
        let profiles = LlmProfiles::default();
        assert_eq!(profiles.profiles.len(), 1);
        assert_eq!(profiles.profiles[0].id.as_str(), CHATGPT_PROFILE_ID);
        assert_eq!(
            profiles.profiles[0].endpoint,
            EndpointConfig::ChatGptSubscription
        );
        profiles.validate().unwrap();
    }
}
