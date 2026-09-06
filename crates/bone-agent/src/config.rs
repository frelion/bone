//! System interruption-review settings and task-local solver selection.

use std::time::Duration;

use bone_config::{ConfigError, ConfigSection, ConfigSnapshot};
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
    #[serde(default = "default_soft_deadline_seconds")]
    #[schemars(range(min = 1))]
    pub soft_deadline_seconds: u32,
    #[serde(default = "default_shutdown_grace_seconds")]
    #[schemars(range(min = 1))]
    pub shutdown_grace_seconds: u32,
}

impl ConfigSection for SystemConfig {
    const KEY: &'static str = "agent.system";

    fn description() -> &'static str {
        "Agent models and runtime deadlines. Loaded when a session starts."
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
            .map_err(|message| format!("default_solver: {message}"))?;
        if self.soft_deadline_seconds == 0 {
            return Err("soft_deadline_seconds must be greater than zero".into());
        }
        if self.shutdown_grace_seconds == 0 {
            return Err("shutdown_grace_seconds must be greater than zero".into());
        }
        Ok(())
    }
}

impl SystemConfig {
    pub fn from_snapshot(snapshot: &ConfigSnapshot) -> Result<Self, ConfigError> {
        snapshot
            .get::<Self>()?
            .ok_or_else(|| ConfigError::InvalidSection {
                section: Self::KEY.into(),
                message: "section is required".into(),
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

fn default_soft_deadline_seconds() -> u32 {
    30
}

fn default_shutdown_grace_seconds() -> u32 {
    5
}

/// The agent uses OpenAI Responses through the subscription adapter.
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
