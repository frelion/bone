//! Validated Agent execution limits, model deadlines, and scheduler capacity.

use std::time::Duration;

use bone_tools::{ToolLimits, ToolLimitsError};
use thiserror::Error;

use crate::KernelConfig;

/// The active deadlines captured by one runtime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuntimeDeadlines {
    kernel_timeout: Duration,
    work_timeout: Duration,
    shutdown_grace_period: Duration,
}

impl RuntimeDeadlines {
    pub fn kernel_timeout(self) -> Duration {
        self.kernel_timeout
    }

    pub fn work_timeout(self) -> Duration {
        self.work_timeout
    }

    pub fn shutdown_grace_period(self) -> Duration {
        self.shutdown_grace_period
    }
}

/// A validated, immutable snapshot of all Agent execution settings.
///
/// The supplied Kernel configuration determines both model timeouts and all
/// scheduler capacities. Model selection and credentials live in AgentModels.
#[derive(Clone, Debug)]
pub struct ResolvedAgentRuntimeConfig {
    tool_limits: ToolLimits,
    kernel_config: KernelConfig,
    shutdown_grace_period: Duration,
}

impl ResolvedAgentRuntimeConfig {
    /// Validate explicit tool limits, Kernel settings, and shutdown grace.
    /// AgentHost forwards these settings without substituting scheduler defaults.
    pub fn new(
        tool_limits: ToolLimits,
        kernel_config: KernelConfig,
        shutdown_grace_period: Duration,
    ) -> Result<Self, ResolvedAgentRuntimeConfigError> {
        tool_limits.validate()?;
        for (field, duration) in [
            ("kernel_timeout", kernel_config.kernel_timeout),
            ("work_timeout", kernel_config.work_timeout),
            ("shutdown_grace_period", shutdown_grace_period),
        ] {
            if duration.is_zero() {
                return Err(ResolvedAgentRuntimeConfigError::NonPositiveDeadline { field });
            }
        }
        for (field, capacity) in [
            (
                "background_concurrency",
                kernel_config.background_concurrency,
            ),
            ("input_capacity", kernel_config.input_capacity),
            ("tool_concurrency", kernel_config.tool_concurrency),
        ] {
            if capacity == 0 {
                return Err(ResolvedAgentRuntimeConfigError::NonPositiveCapacity { field });
            }
        }
        Ok(Self {
            tool_limits,
            kernel_config,
            shutdown_grace_period,
        })
    }

    pub fn tool_limits(&self) -> &ToolLimits {
        &self.tool_limits
    }

    pub fn kernel_config(&self) -> &KernelConfig {
        &self.kernel_config
    }

    pub fn deadlines(&self) -> RuntimeDeadlines {
        RuntimeDeadlines {
            kernel_timeout: self.kernel_config.kernel_timeout,
            work_timeout: self.kernel_config.work_timeout,
            shutdown_grace_period: self.shutdown_grace_period,
        }
    }
}

/// A failure while validating Agent execution settings.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ResolvedAgentRuntimeConfigError {
    #[error(transparent)]
    ToolLimits(#[from] ToolLimitsError),
    #[error("{field} must be greater than zero")]
    NonPositiveDeadline { field: &'static str },
    #[error("{field} must be greater than zero")]
    NonPositiveCapacity { field: &'static str },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kernel_config() -> KernelConfig {
        KernelConfig {
            kernel_timeout: Duration::from_secs(15),
            work_timeout: Duration::from_secs(17),
            background_concurrency: 3,
            input_capacity: 7,
            tool_concurrency: 4,
        }
    }

    #[test]
    fn captures_explicit_deadlines_and_capacity_without_sharing_mutable_settings() {
        let mut kernel = kernel_config();
        let config = ResolvedAgentRuntimeConfig::new(
            ToolLimits::default(),
            kernel.clone(),
            Duration::from_secs(2),
        )
        .unwrap();
        kernel.kernel_timeout = Duration::from_secs(100);
        kernel.background_concurrency = 99;
        assert_ne!(config.kernel_config().kernel_timeout, kernel.kernel_timeout);
        assert_ne!(
            config.kernel_config().background_concurrency,
            kernel.background_concurrency
        );
        assert_eq!(config.deadlines().kernel_timeout(), Duration::from_secs(15));
        assert_eq!(config.deadlines().work_timeout(), Duration::from_secs(17));
        assert_eq!(
            config.deadlines().shutdown_grace_period(),
            Duration::from_secs(2)
        );
        assert_eq!(config.kernel_config().background_concurrency, 3);
        assert_eq!(config.kernel_config().input_capacity, 7);
        assert_eq!(config.kernel_config().tool_concurrency, 4);
    }

    #[test]
    fn rejects_each_zero_deadline_and_capacity() {
        for field in ["kernel_timeout", "work_timeout", "shutdown_grace_period"] {
            let mut kernel = kernel_config();
            let mut grace = Duration::from_secs(2);
            match field {
                "kernel_timeout" => kernel.kernel_timeout = Duration::ZERO,
                "work_timeout" => kernel.work_timeout = Duration::ZERO,
                _ => grace = Duration::ZERO,
            }
            assert_eq!(
                ResolvedAgentRuntimeConfig::new(ToolLimits::default(), kernel, grace).unwrap_err(),
                ResolvedAgentRuntimeConfigError::NonPositiveDeadline { field },
            );
        }
        for field in [
            "background_concurrency",
            "input_capacity",
            "tool_concurrency",
        ] {
            let mut kernel = kernel_config();
            match field {
                "background_concurrency" => kernel.background_concurrency = 0,
                "input_capacity" => kernel.input_capacity = 0,
                _ => kernel.tool_concurrency = 0,
            }
            assert_eq!(
                ResolvedAgentRuntimeConfig::new(
                    ToolLimits::default(),
                    kernel,
                    Duration::from_secs(2)
                )
                .unwrap_err(),
                ResolvedAgentRuntimeConfigError::NonPositiveCapacity { field },
            );
        }
    }

    #[test]
    fn validates_tool_limits_before_starting_a_runtime() {
        let limits = ToolLimits {
            max_read_lines: 0,
            ..ToolLimits::default()
        };
        assert!(matches!(
            ResolvedAgentRuntimeConfig::new(limits, kernel_config(), Duration::from_secs(2)),
            Err(ResolvedAgentRuntimeConfigError::ToolLimits(_)),
        ));
    }
}
