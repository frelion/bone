//! System interruption-review settings and task-local solver selection.

use std::{path::Path, time::Duration};

use bone_config::{ConfigError, ConfigManager, ConfigSection};
use bone_llm::protocol::openai_responses;
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Settings owned by the application host, never by task input.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SystemConfig {
    pub coordinator: ModelSettings,
    pub default_solver: ModelSettings,
}

impl ConfigSection for SystemConfig {
    const KEY: &'static str = "agent.system";

    fn description() -> &'static str {
        "System interruption reviewer and default solver. Loaded when a session starts."
    }

    fn schema() -> Value {
        schema_for!(Self).to_value()
    }

    fn validate(&self) -> Result<(), String> {
        self.coordinator
            .validate()
            .map_err(|message| format!("coordinator: {message}"))?;
        self.default_solver
            .validate()
            .map_err(|message| format!("default_solver: {message}"))
    }
}

impl SystemConfig {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        ConfigManager::builder()
            .register::<Self>()?
            .build(path)?
            .snapshot()?
            .get::<Self>()?
            .ok_or_else(|| ConfigError::InvalidSection {
                section: Self::KEY.into(),
                message: "section is required; run `bone --help` for setup".into(),
            })
    }

    /// Resolves a task's solver without mutating the system defaults.
    pub fn solver_for(&self, task: &TaskConfig) -> Result<ModelSettings, String> {
        let mut solver = self.default_solver.clone();
        if let Some(model) = &task.model {
            solver.model = model.clone();
        }
        if let Some(effort) = task.effort {
            solver.effort = Some(effort);
        }
        if let Some(timeout_seconds) = task.timeout_seconds {
            solver.timeout_seconds = timeout_seconds;
        }
        solver.validate()?;
        Ok(solver)
    }
}

/// Task settings expose only solver overrides. Coordinator settings stay with
/// the host even when a caller supplies a serialized task configuration.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskConfig {
    pub model: Option<String>,
    pub effort: Option<Effort>,
    pub timeout_seconds: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelSettings {
    #[schemars(length(min = 1))]
    pub model: String,
    /// OpenAI Responses reasoning effort. Omitted means provider default.
    pub effort: Option<Effort>,
    #[serde(default = "default_timeout_seconds")]
    #[schemars(range(min = 1))]
    pub timeout_seconds: u32,
}

impl ModelSettings {
    pub fn timeout(&self) -> Duration {
        Duration::from_secs(u64::from(self.timeout_seconds))
    }

    fn validate(&self) -> Result<(), String> {
        if self.model.trim().is_empty() || self.model.trim() != self.model {
            return Err("model must be non-empty without surrounding whitespace".into());
        }
        if self.timeout_seconds == 0 {
            return Err("timeout_seconds must be greater than zero".into());
        }
        Ok(())
    }
}

fn default_timeout_seconds() -> u32 {
    120
}

/// The current CLI uses OpenAI Responses through the subscription adapter.
/// Unsupported model/effort combinations are reported by that provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    None,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl From<Effort> for openai_responses::ReasoningEffort {
    fn from(effort: Effort) -> Self {
        match effort {
            Effort::None => Self::None,
            Effort::Minimal => Self::Minimal,
            Effort::Low => Self::Low,
            Effort::Medium => Self::Medium,
            Effort::High => Self::High,
            Effort::Xhigh => Self::Xhigh,
            Effort::Max => Self::Max,
        }
    }
}
