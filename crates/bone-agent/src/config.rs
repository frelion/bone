use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The finite resources and deadlines of one agent runtime.
///
/// Start from [`AgentLimits::default`], change the fields that matter to the
/// host, then call [`AgentLimits::validate`] before displaying configuration
/// errors. The runtime performs the same validation when it starts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentLimits {
    pub coordination_timeout: Duration,
    pub work_timeout: Duration,
    pub tool_timeout: Duration,
    pub shutdown_grace: Duration,
    pub background_workers: usize,
    pub tool_slots: usize,
    pub active_jobs: usize,
    pub job_depth: usize,
    pub pending_inputs: usize,
    pub inquiries: usize,
    /// Acceptance ceiling for a serialized context DTO or complete original
    /// model result.
    ///
    /// Provider instructions and tool schemas are outside the context-side
    /// count, so hosts must leave suitable model-window headroom. An oversized
    /// untrusted model result is replaced by a fixed diagnostic. That
    /// diagnostic is globally bounded but is not guaranteed to fit an
    /// arbitrarily tiny configured value.
    pub context_bytes: usize,
    /// Acceptance ceiling for one original model-authored item stored by the
    /// kernel.
    pub item_bytes: usize,
    /// Acceptance ceiling for a complete original successful or failed tool
    /// outcome.
    ///
    /// An oversized outcome becomes a fixed failure while preserving its
    /// external-effect classification. The replacement is globally bounded
    /// but is not guaranteed to fit an arbitrarily tiny configured value.
    pub tool_output_bytes: usize,
}

impl AgentLimits {
    pub fn validate(&self) -> Result<(), AgentLimitsError> {
        for (name, value) in [
            ("coordination_timeout", self.coordination_timeout),
            ("work_timeout", self.work_timeout),
            ("tool_timeout", self.tool_timeout),
            ("shutdown_grace", self.shutdown_grace),
        ] {
            if value.is_zero() {
                return Err(AgentLimitsError::Zero(name));
            }
        }
        for (name, value) in [
            ("background_workers", self.background_workers),
            ("tool_slots", self.tool_slots),
            ("active_jobs", self.active_jobs),
            ("job_depth", self.job_depth),
            ("pending_inputs", self.pending_inputs),
            ("inquiries", self.inquiries),
            ("context_bytes", self.context_bytes),
            ("item_bytes", self.item_bytes),
            ("tool_output_bytes", self.tool_output_bytes),
        ] {
            if value == 0 {
                return Err(AgentLimitsError::Zero(name));
            }
        }
        if self.item_bytes > self.context_bytes {
            return Err(AgentLimitsError::ItemExceedsContext);
        }
        Ok(())
    }

    pub(crate) fn worker_slots(&self) -> usize {
        self.background_workers.saturating_add(1)
    }
}

impl Default for AgentLimits {
    fn default() -> Self {
        Self {
            coordination_timeout: Duration::from_secs(30),
            work_timeout: Duration::from_secs(300),
            tool_timeout: Duration::from_secs(120),
            shutdown_grace: Duration::from_secs(5),
            background_workers: 2,
            tool_slots: 8,
            active_jobs: 64,
            job_depth: 8,
            pending_inputs: 32,
            inquiries: 32,
            context_bytes: 96 * 1024,
            item_bytes: 16 * 1024,
            tool_output_bytes: 1024 * 1024,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AgentLimitsError {
    #[error("{0} must be greater than zero")]
    Zero(&'static str),
    #[error("item_bytes cannot exceed context_bytes")]
    ItemExceedsContext,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_each_kind_of_limit() {
        let mut limits = AgentLimits {
            context_bytes: 0,
            ..AgentLimits::default()
        };
        assert_eq!(
            limits.validate(),
            Err(AgentLimitsError::Zero("context_bytes"))
        );

        limits.context_bytes = 1;
        assert_eq!(limits.validate(), Err(AgentLimitsError::ItemExceedsContext));

        limits.item_bytes = 1;
        assert_eq!(limits.validate(), Ok(()));
        assert_eq!(limits.worker_slots(), 3);

        limits.background_workers = usize::MAX;
        assert_eq!(limits.worker_slots(), usize::MAX);
    }

    #[test]
    fn limits_round_trip_through_json() {
        let limits = AgentLimits::default();
        let encoded = serde_json::to_string(&limits).unwrap();
        assert_eq!(
            serde_json::from_str::<AgentLimits>(&encoded).unwrap(),
            limits
        );
    }
}
