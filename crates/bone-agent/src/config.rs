//! Validated Agent runtime configuration.
//!
//! This module contains only the Agent's own execution limits and deadlines.
//! Product code resolves its settings, constructs role models, and supplies a
//! [`ResolvedAgentRuntimeConfig`] to [`crate::AgentHost`] at runtime startup.

use std::time::Duration;

use bone_tools::{ToolLimits, ToolLimitsError};
use thiserror::Error;

/// All timeouts captured by one runtime.
///
/// The type is immutable so an attached runtime cannot observe later product
/// settings changes.
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

/// A complete, validated, immutable snapshot of Agent-owned runtime settings.
///
/// Model choice, provider protocol options, credentials, and persisted
/// settings locations deliberately do not appear here. They are resolved by
/// the product into the [`crate::AgentModels`] supplied to [`crate::AgentHost`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedAgentRuntimeConfig {
    tool_limits: ToolLimits,
    deadlines: RuntimeDeadlines,
}

impl ResolvedAgentRuntimeConfig {
    /// Construct a complete runtime snapshot from already-resolved Agent
    /// execution settings.
    pub fn new(
        tool_limits: ToolLimits,
        soft_deadline: Duration,
        review_timeout: Duration,
        work_timeout: Duration,
        shutdown_grace_period: Duration,
    ) -> Result<Self, ResolvedAgentRuntimeConfigError> {
        tool_limits
            .validate()
            .map_err(ResolvedAgentRuntimeConfigError::ToolLimits)?;
        for (field, duration) in [
            ("soft_deadline", soft_deadline),
            ("review_timeout", review_timeout),
            ("work_timeout", work_timeout),
            ("shutdown_grace_period", shutdown_grace_period),
        ] {
            if duration.is_zero() {
                return Err(ResolvedAgentRuntimeConfigError::NonPositiveDeadline { field });
            }
        }
        Ok(Self {
            tool_limits,
            deadlines: RuntimeDeadlines {
                soft_deadline,
                review_timeout,
                work_timeout,
                shutdown_grace_period,
            },
        })
    }

    pub fn tool_limits(&self) -> &ToolLimits {
        &self.tool_limits
    }

    pub fn deadlines(&self) -> RuntimeDeadlines {
        self.deadlines
    }
}

/// A failure while converting product settings into Agent runtime settings.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ResolvedAgentRuntimeConfigError {
    #[error(transparent)]
    ToolLimits(#[from] ToolLimitsError),
    #[error("{field} must be greater than zero")]
    NonPositiveDeadline { field: &'static str },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> ResolvedAgentRuntimeConfig {
        ResolvedAgentRuntimeConfig::new(
            ToolLimits::default(),
            Duration::from_secs(3),
            Duration::from_secs(15),
            Duration::from_secs(17),
            Duration::from_secs(2),
        )
        .unwrap()
    }

    #[test]
    fn captures_only_agent_execution_settings() {
        let config = config();
        assert_eq!(config.deadlines().soft_deadline(), Duration::from_secs(3));
        assert_eq!(config.deadlines().review_timeout(), Duration::from_secs(15));
        assert_eq!(config.deadlines().work_timeout(), Duration::from_secs(17));
        assert_eq!(
            config.deadlines().shutdown_grace_period(),
            Duration::from_secs(2)
        );
    }

    #[test]
    fn rejects_each_non_positive_deadline() {
        for field in [
            "soft_deadline",
            "review_timeout",
            "work_timeout",
            "shutdown_grace_period",
        ] {
            let mut values = [
                Duration::from_secs(1),
                Duration::from_secs(1),
                Duration::from_secs(1),
                Duration::from_secs(1),
            ];
            values[[
                "soft_deadline",
                "review_timeout",
                "work_timeout",
                "shutdown_grace_period",
            ]
            .iter()
            .position(|candidate| *candidate == field)
            .unwrap()] = Duration::ZERO;
            assert_eq!(
                ResolvedAgentRuntimeConfig::new(
                    ToolLimits::default(),
                    values[0],
                    values[1],
                    values[2],
                    values[3],
                ),
                Err(ResolvedAgentRuntimeConfigError::NonPositiveDeadline { field })
            );
        }
    }
}
