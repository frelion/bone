use std::path::PathBuf;

use bone_config::ConfigSection;
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::service::chatgpt_subscription;

/// LLM service settings read when a session starts.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct LlmConfig {
    /// Absolute directory for BONE's independent subscription credentials.
    /// Omit to use the conventional BONE credential directory.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential_root: Option<PathBuf>,
}

impl ConfigSection for LlmConfig {
    const KEY: &'static str = "llm.system";

    fn description() -> &'static str {
        "LLM service settings. Applied when a session starts."
    }

    fn schema() -> Value {
        schema_for!(Self).to_value()
    }

    fn validate(&self) -> Result<(), String> {
        if let Some(root) = &self.credential_root {
            if !root.is_absolute() {
                return Err("credential_root must be an absolute path".into());
            }
            if root.to_string_lossy().contains('\0') {
                return Err("credential_root must not contain null bytes".into());
            }
        }
        Ok(())
    }
}

impl LlmConfig {
    /// Resolve the credential directory without creating files or starting login.
    pub fn resolve_credential_root(&self) -> Result<PathBuf, chatgpt_subscription::Error> {
        self.validate()
            .map_err(|_| chatgpt_subscription::Error::CredentialStoreUnavailable)?;
        match &self.credential_root {
            Some(root) => Ok(root.clone()),
            None => chatgpt_subscription::default_credential_root(),
        }
    }
}
