//! Validated Agent runtime configuration.
//!
//! This module contains only domain values and deterministic resolution. It
//! deliberately does not know where settings came from, how they are persisted,
//! or how an endpoint is authenticated. Product code resolves stored settings
//! into [`ResolvedAgentRuntimeConfig`] before it asks [`crate::AgentHost`] to
//! create a runtime.

use std::{fmt, time::Duration};

use bone_tools::{ToolLimits, ToolLimitsError};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

const DEFAULT_TIMEOUT_SECONDS: u32 = 120;
const DEFAULT_SOFT_DEADLINE_SECONDS: u32 = 30;
const DEFAULT_SHUTDOWN_GRACE_SECONDS: u32 = 5;

/// Persistable Agent defaults supplied by the product settings layer.
///
/// `SystemConfig` has no storage registration of its own. It is a plain
/// validated domain value that the product can keep in SQLite, a test fixture,
/// or another settings backend.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SystemConfig {
    pub coordinator: ModelSettings,
    pub default_solver: ModelSettings,
    #[serde(default = "default_soft_deadline_seconds")]
    pub soft_deadline_seconds: u32,
    #[serde(default = "default_shutdown_grace_seconds")]
    pub shutdown_grace_seconds: u32,
}

impl SystemConfig {
    /// Validate persisted defaults before exposing them to a product settings
    /// UI or resolving a runtime configuration.
    pub fn validate(&self) -> Result<(), SystemConfigError> {
        self.coordinator
            .validate()
            .map_err(SystemConfigError::Coordinator)?;
        self.default_solver
            .validate()
            .map_err(SystemConfigError::DefaultSolver)?;
        if self.soft_deadline_seconds == 0 {
            return Err(SystemConfigError::NonPositive {
                field: "soft_deadline_seconds",
            });
        }
        if self.shutdown_grace_seconds == 0 {
            return Err(SystemConfigError::NonPositive {
                field: "shutdown_grace_seconds",
            });
        }
        Ok(())
    }

    /// Resolve one immutable Agent runtime configuration.
    ///
    /// A caller may replace the complete solver settings for this runtime,
    /// while coordinator settings remain system-owned. The returned value is a
    /// complete pinned snapshot: later settings changes cannot affect a
    /// runtime that was already started with it.
    pub fn resolve(
        &self,
        tool_limits: ToolLimits,
        solver_override: Option<ModelSettings>,
    ) -> Result<ResolvedAgentRuntimeConfig, ResolvedAgentRuntimeConfigError> {
        let solver = solver_override.unwrap_or_else(|| self.default_solver.clone());
        ResolvedAgentRuntimeConfig::new(
            self.coordinator.clone(),
            solver,
            tool_limits,
            Duration::from_secs(u64::from(self.soft_deadline_seconds)),
            Duration::from_secs(u64::from(self.shutdown_grace_seconds)),
        )
    }
}

/// A validation failure in [`SystemConfig`].
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum SystemConfigError {
    #[error("coordinator: {0}")]
    Coordinator(#[source] ModelSettingsError),
    #[error("default_solver: {0}")]
    DefaultSolver(#[source] ModelSettingsError),
    #[error("{field} must be greater than zero")]
    NonPositive { field: &'static str },
}

/// One model selection together with its request deadline.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelSettings {
    pub model: String,
    /// OpenAI Responses reasoning effort. Omitted means provider default.
    pub effort: Option<Effort>,
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: u32,
}

impl ModelSettings {
    pub fn validate(&self) -> Result<(), ModelSettingsError> {
        if self.model.trim().is_empty() || self.model.trim() != self.model {
            return Err(ModelSettingsError::InvalidModel);
        }
        if self.timeout_seconds == 0 {
            return Err(ModelSettingsError::ZeroTimeout);
        }
        Ok(())
    }

    pub fn timeout(&self) -> Duration {
        Duration::from_secs(u64::from(self.timeout_seconds))
    }
}

/// A validation failure in [`ModelSettings`].
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum ModelSettingsError {
    #[error("model must be non-empty without surrounding whitespace")]
    InvalidModel,
    #[error("timeout_seconds must be greater than zero")]
    ZeroTimeout,
}

fn default_timeout_seconds() -> u32 {
    DEFAULT_TIMEOUT_SECONDS
}

fn default_soft_deadline_seconds() -> u32 {
    DEFAULT_SOFT_DEADLINE_SECONDS
}

fn default_shutdown_grace_seconds() -> u32 {
    DEFAULT_SHUTDOWN_GRACE_SECONDS
}

/// The agent uses OpenAI Responses through its injected endpoint. Unsupported
/// model/effort combinations are reported by that provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

impl From<Effort> for bone_llm::protocol::openai_responses::ReasoningEffort {
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

/// All timeouts captured by one runtime. The type is immutable and is derived
/// from the selected model settings plus system deadlines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuntimeDeadlines {
    soft_deadline: Duration,
    review_timeout: Duration,
    work_timeout: Duration,
    shutdown_grace_period: Duration,
}

impl RuntimeDeadlines {
    pub fn soft_deadline(self) -> Duration {
        self.soft_deadline
    }

    pub fn review_timeout(self) -> Duration {
        self.review_timeout
    }

    pub fn work_timeout(self) -> Duration {
        self.work_timeout
    }

    pub fn shutdown_grace_period(self) -> Duration {
        self.shutdown_grace_period
    }
}

/// Stable SHA-256 identity of every value that affects an Agent runtime.
///
/// This is deliberately an opaque value. It is safe to persist in a journal or
/// display in diagnostics, but it is not a capability or a credential.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct RuntimeConfigFingerprint([u8; 32]);

impl RuntimeConfigFingerprint {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Display for RuntimeConfigFingerprint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for RuntimeConfigFingerprint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("RuntimeConfigFingerprint")
            .field(&self.to_string())
            .finish()
    }
}

/// A complete, validated, immutable snapshot consumed by [`crate::AgentHost`].
///
/// Keeping this object separate from `SystemConfig` prevents a runtime from
/// reading settings while it works. The product resolves a fresh instance at a
/// deliberate startup boundary; the Agent then only receives this pinned value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedAgentRuntimeConfig {
    coordinator: ModelSettings,
    solver: ModelSettings,
    tool_limits: ToolLimits,
    deadlines: RuntimeDeadlines,
    fingerprint: RuntimeConfigFingerprint,
}

impl ResolvedAgentRuntimeConfig {
    /// Construct a complete runtime snapshot from already-selected values.
    pub fn new(
        coordinator: ModelSettings,
        solver: ModelSettings,
        tool_limits: ToolLimits,
        soft_deadline: Duration,
        shutdown_grace_period: Duration,
    ) -> Result<Self, ResolvedAgentRuntimeConfigError> {
        coordinator
            .validate()
            .map_err(ResolvedAgentRuntimeConfigError::Coordinator)?;
        solver
            .validate()
            .map_err(ResolvedAgentRuntimeConfigError::Solver)?;
        tool_limits
            .validate()
            .map_err(ResolvedAgentRuntimeConfigError::ToolLimits)?;
        if soft_deadline.is_zero() {
            return Err(ResolvedAgentRuntimeConfigError::NonPositiveDeadline {
                field: "soft_deadline",
            });
        }
        if shutdown_grace_period.is_zero() {
            return Err(ResolvedAgentRuntimeConfigError::NonPositiveDeadline {
                field: "shutdown_grace_period",
            });
        }

        let deadlines = RuntimeDeadlines {
            soft_deadline,
            review_timeout: coordinator.timeout(),
            work_timeout: solver.timeout(),
            shutdown_grace_period,
        };
        let fingerprint = stable_fingerprint(&coordinator, &solver, &tool_limits, deadlines);
        Ok(Self {
            coordinator,
            solver,
            tool_limits,
            deadlines,
            fingerprint,
        })
    }

    pub fn coordinator(&self) -> &ModelSettings {
        &self.coordinator
    }

    pub fn solver(&self) -> &ModelSettings {
        &self.solver
    }

    pub fn tool_limits(&self) -> &ToolLimits {
        &self.tool_limits
    }

    pub fn deadlines(&self) -> RuntimeDeadlines {
        self.deadlines
    }

    pub fn fingerprint(&self) -> RuntimeConfigFingerprint {
        self.fingerprint
    }
}

/// A failure while converting persisted/product settings into a runtime
/// snapshot.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ResolvedAgentRuntimeConfigError {
    #[error("coordinator: {0}")]
    Coordinator(#[source] ModelSettingsError),
    #[error("solver: {0}")]
    Solver(#[source] ModelSettingsError),
    #[error(transparent)]
    ToolLimits(#[from] ToolLimitsError),
    #[error("{field} must be greater than zero")]
    NonPositiveDeadline { field: &'static str },
}

fn stable_fingerprint(
    coordinator: &ModelSettings,
    solver: &ModelSettings,
    limits: &ToolLimits,
    deadlines: RuntimeDeadlines,
) -> RuntimeConfigFingerprint {
    let mut hasher = Sha256::new();
    update_bytes(&mut hasher, b"bone-agent.resolved-runtime-config.v1");
    update_model(&mut hasher, coordinator);
    update_model(&mut hasher, solver);
    for value in [
        limits.max_output_bytes as u64,
        limits.max_read_lines as u64,
        limits.max_read_file_bytes,
        limits.max_glob_results as u64,
        limits.max_grep_matches as u64,
        limits.max_grep_pattern_bytes as u64,
        limits.max_grep_line_chars as u64,
        limits.max_search_file_bytes,
        limits.max_search_total_bytes,
        limits.max_ignore_file_bytes,
        limits.max_ignore_total_bytes,
        limits.max_walk_entries as u64,
        limits.max_patch_bytes as u64,
        limits.max_patch_files as u64,
        limits.max_patch_file_bytes,
        limits.max_patch_total_bytes,
        limits.max_bash_command_bytes as u64,
    ] {
        update_u64(&mut hasher, value);
    }
    update_duration(&mut hasher, limits.default_bash_timeout);
    update_duration(&mut hasher, limits.max_bash_timeout);
    update_duration(&mut hasher, deadlines.soft_deadline);
    update_duration(&mut hasher, deadlines.review_timeout);
    update_duration(&mut hasher, deadlines.work_timeout);
    update_duration(&mut hasher, deadlines.shutdown_grace_period);
    RuntimeConfigFingerprint(hasher.finalize().into())
}

fn update_model(hasher: &mut Sha256, model: &ModelSettings) {
    update_bytes(hasher, model.model.as_bytes());
    match model.effort {
        None => hasher.update([0]),
        Some(effort) => {
            hasher.update([1, effort_tag(effort)]);
        }
    }
    update_u64(hasher, u64::from(model.timeout_seconds));
}

fn effort_tag(effort: Effort) -> u8 {
    match effort {
        Effort::None => 0,
        Effort::Minimal => 1,
        Effort::Low => 2,
        Effort::Medium => 3,
        Effort::High => 4,
        Effort::Xhigh => 5,
        Effort::Max => 6,
    }
}

fn update_duration(hasher: &mut Sha256, duration: Duration) {
    update_u64(hasher, duration.as_secs());
    hasher.update(duration.subsec_nanos().to_le_bytes());
}

fn update_u64(hasher: &mut Sha256, value: u64) {
    hasher.update(value.to_le_bytes());
}

fn update_bytes(hasher: &mut Sha256, value: &[u8]) {
    update_u64(hasher, value.len() as u64);
    hasher.update(value);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn system() -> SystemConfig {
        SystemConfig {
            coordinator: ModelSettings {
                model: "reviewer".into(),
                effort: Some(Effort::Low),
                timeout_seconds: 15,
            },
            default_solver: ModelSettings {
                model: "solver".into(),
                effort: Some(Effort::High),
                timeout_seconds: 17,
            },
            soft_deadline_seconds: 3,
            shutdown_grace_seconds: 2,
        }
    }

    #[test]
    fn resolving_an_override_never_mutates_system_defaults() {
        let system = system();
        let runtime = system
            .resolve(
                ToolLimits::default(),
                Some(ModelSettings {
                    model: "session-solver".into(),
                    effort: Some(Effort::Max),
                    timeout_seconds: 300,
                }),
            )
            .unwrap();

        assert_eq!(runtime.coordinator().model, "reviewer");
        assert_eq!(runtime.solver().model, "session-solver");
        assert_eq!(
            runtime.deadlines().review_timeout(),
            Duration::from_secs(15)
        );
        assert_eq!(runtime.deadlines().work_timeout(), Duration::from_secs(300));
        assert_eq!(system.default_solver.model, "solver");
    }

    #[test]
    fn fingerprints_are_stable_and_cover_every_runtime_input() {
        let system = system();
        let first = system.resolve(ToolLimits::default(), None).unwrap();
        let second = system.resolve(ToolLimits::default(), None).unwrap();
        assert_eq!(first.fingerprint(), second.fingerprint());
        assert_eq!(first.fingerprint().to_string().len(), 64);

        let changed = system
            .resolve(
                ToolLimits {
                    max_read_lines: 1,
                    ..ToolLimits::default()
                },
                None,
            )
            .unwrap();
        assert_ne!(first.fingerprint(), changed.fingerprint());
    }

    #[test]
    fn invalid_system_values_fail_before_runtime_creation() {
        let mut invalid = system();
        invalid.soft_deadline_seconds = 0;
        assert!(invalid.validate().is_err());
        assert!(invalid.resolve(ToolLimits::default(), None).is_err());
    }
}
